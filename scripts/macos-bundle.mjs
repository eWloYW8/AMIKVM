import { spawnSync } from 'node:child_process';
import { cpSync, existsSync, lstatSync, mkdtempSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync } from 'node:fs';
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from 'node:path';

const architectures = new Map([[0x01000007, 'x64'], [0x0100000c, 'arm64']]);
const libraryCommands = new Set([0xc, 0x80000018, 0x8000001f, 0x20, 0x80000023]);
const systemLibrary = name => isAbsolute(name) && ['/System/Library/', '/usr/lib/'].some(prefix => resolve(name).startsWith(prefix));
const version = value => {
  if (typeof value !== 'string' || !/^\d+(\.\d+){0,2}$/.test(value)) throw new Error(`Invalid macOS version: ${value}`);
  const parts = value.split('.').map(Number).concat([0, 0]).slice(0, 3);
  if (parts.some(v => !Number.isSafeInteger(v) || v > 65535)) throw new Error(`Invalid macOS version: ${value}`);
  return parts;
};
const newer = (a, b) => a.some((value, index) => value !== b[index] && a.slice(0, index).every((v, i) => v === b[i]) && value > b[index]);

// Inspect the actual Mach-O file, including every universal-binary slice.
export function inspectMacosBinary(path) {
  const data = readFileSync(path);
  function range(offset, length, limit = data.length) {
    if (!Number.isSafeInteger(offset) || !Number.isSafeInteger(length) || offset < 0 || length < 0 || offset + length > limit) {
      throw new Error(`Invalid Mach-O range in ${path}`);
    }
  }
  const u32 = (offset, little = true) => { range(offset, 4); return little ? data.readUInt32LE(offset) : data.readUInt32BE(offset); };
  const u64 = (offset, little = true) => {
    range(offset, 8);
    const value = little ? data.readBigUInt64LE(offset) : data.readBigUInt64BE(offset);
    if (value > BigInt(Number.MAX_SAFE_INTEGER)) throw new Error(`Oversized Mach-O offset in ${path}`);
    return Number(value);
  };
  let slices = [{ offset: 0, size: data.length }];
  const magic = u32(0, false);
  if ([0xcafebabe, 0xcafebabf, 0xbebafeca, 0xbfbafeca].includes(magic)) {
    const little = magic === 0xbebafeca || magic === 0xbfbafeca;
    const wide = magic === 0xcafebabf || magic === 0xbfbafeca;
    const count = u32(4, little), stride = wide ? 32 : 20, end = 8 + count * stride;
    if (!count || count > 32) throw new Error('Invalid universal Mach-O architecture count');
    range(0, end);
    slices = Array.from({ length: count }, (_, i) => {
      const at = 8 + i * stride;
      const offset = wide ? u64(at + 8, little) : u32(at + 8, little);
      const size = wide ? u64(at + 16, little) : u32(at + 12, little);
      const alignment = u32(at + (wide ? 24 : 16), little);
      if (offset < end || !size || alignment > 31 || offset % 2 ** alignment !== 0 || (wide && u32(at + 28, little))) {
        throw new Error('Invalid universal Mach-O slice');
      }
      range(offset, size);
      return { offset, size, cpu: u32(at, little), subtype: u32(at + 4, little) };
    });
    const sorted = [...slices].sort((a, b) => a.offset - b.offset);
    if (sorted.some((s, i) => i && sorted[i - 1].offset + sorted[i - 1].size > s.offset)) throw new Error('Overlapping Mach-O slices');
  }
  const seen = new Set();
  return slices.map(slice => {
    const { offset, size } = slice, end = offset + size;
    range(offset, 32, end);
    if (u32(offset) !== 0xfeedfacf) throw new Error('macOS packages require a 64-bit little-endian Mach-O binary');
    const cpu = u32(offset + 4), subtype = u32(offset + 8), architecture = architectures.get(cpu);
    if (!architecture || (slice.cpu !== undefined && (slice.cpu !== cpu || slice.subtype !== subtype)) || seen.has(architecture)) {
      throw new Error('Unsupported, duplicate or mismatched Mach-O architecture');
    }
    seen.add(architecture);
    const filetype = u32(offset + 12), count = u32(offset + 16), commandEnd = offset + 32 + u32(offset + 20);
    if (![2, 6].includes(filetype) || !count || count > 4096) throw new Error('Mach-O must be an executable or dynamic library');
    range(offset + 32, commandEnd - offset - 32, end);
    let at = offset + 32, minimum, entry = false;
    const imports = [], rpaths = [];
    for (let i = 0; i < count; i++) {
      range(at, 8, commandEnd);
      const command = u32(at), length = u32(at + 4);
      if (length < 8 || length % 8) throw new Error('Invalid Mach-O load command size');
      range(at, length, commandEnd);
      function string(field, minimumOffset) {
        const start = at + u32(at + field), stop = data.indexOf(0, start);
        if (start < at + minimumOffset || start >= at + length || stop < start || stop >= at + length) throw new Error('Invalid Mach-O load path');
        const value = new TextDecoder('utf-8', { fatal: true }).decode(data.subarray(start, stop));
        if (!value) throw new Error('Empty Mach-O load path');
        return value;
      }
      if (libraryCommands.has(command)) {
        if (length < 24) throw new Error('Truncated Mach-O library command');
        imports.push(string(8, 24));
      } else if (command === 0x8000001c) {
        if (length < 16) throw new Error('Truncated Mach-O run path');
        rpaths.push(string(8, 12));
      } else if (command === 0x32 || command === 0x24) {
        if (minimum || length < (command === 0x32 ? 24 : 16)) throw new Error('Invalid Mach-O deployment version');
        if (command === 0x32 && (u32(at + 8) !== 1 || 24 + u32(at + 20) * 8 !== length)) throw new Error('Mach-O is not a macOS desktop build');
        const value = u32(at + (command === 0x32 ? 12 : 8));
        minimum = [value >>> 16, (value >>> 8) & 255, value & 255];
      } else if (command === 0x80000028) {
        if (length !== 24 || u64(at + 8) >= size) throw new Error('Invalid Mach-O entry point');
        entry = true;
      } else if (command === 5) {
        entry = true; // Older LC_UNIXTHREAD executables are still legitimate.
      }
      at += length;
    }
    if (at !== commandEnd || !minimum || (filetype === 2 && !entry)) throw new Error('Incomplete Mach-O load commands');
    return { architecture, filetype, minimum, imports, rpaths };
  });
}

export function validateMacosBundle(path, expectedArchitecture, expected) {
  const root = realpathSync(path);
  if (!statSync(root).isDirectory() || !path.endsWith('.app')) throw new Error('macOS delivery must be an .app folder');
  function contained(file) {
    const actual = realpathSync(file), part = relative(root, actual);
    if (part === '..' || part.startsWith(`..${sep}`) || isAbsolute(part)) throw new Error(`App file points outside the bundle: ${file}`);
    return actual;
  }
  let symbolicLinks = 0;
  const folders = [root];
  while (folders.length) {
    const folder = folders.pop();
    for (const entry of readdirSync(folder, { withFileTypes: true })) {
      const file = join(folder, entry.name);
      if (entry.isDirectory()) folders.push(file);
      else if (entry.isSymbolicLink()) { contained(file); symbolicLinks++; }
      else if (!entry.isFile()) throw new Error(`Unsupported file in app bundle: ${file}`);
    }
  }
  function inside(file) {
    const actual = contained(file);
    if (!statSync(actual).isFile()) throw new Error(`App file is not a regular file: ${file}`);
    return actual;
  }
  const plist = inside(join(root, 'Contents', 'Info.plist'));
  const parsed = spawnSync('plutil', ['-convert', 'json', '-o', '-', plist], { encoding: 'utf8' });
  if (parsed.error || parsed.status !== 0) throw new Error(`Cannot read app Info.plist: ${parsed.error?.message ?? parsed.stderr}`);
  const info = JSON.parse(parsed.stdout);
  if (info.CFBundlePackageType !== 'APPL' || info.CFBundleIdentifier !== expected.identifier || info.CFBundleShortVersionString !== expected.version) {
    throw new Error('macOS app identity/version does not match the project');
  }
  const name = info.CFBundleExecutable, icon = info.CFBundleIconFile;
  if (typeof name !== 'string' || !name || /[/\\\0]/.test(name) || ['.', '..'].includes(name)) throw new Error('Invalid app executable name');
  if (typeof icon !== 'string' || !icon || /[/\\\0]/.test(icon) || ['.', '..'].includes(icon)) throw new Error('Invalid app icon name');
  const iconData = readFileSync(inside(join(root, 'Contents', 'Resources', icon)));
  if (iconData.length < 8 || iconData.toString('ascii', 0, 4) !== 'icns' || iconData.readUInt32BE(4) !== iconData.length) throw new Error('Invalid app ICNS resource');
  const declaredMinimum = version(info.LSMinimumSystemVersion);
  if (expected.minimumSystemVersion && info.LSMinimumSystemVersion !== expected.minimumSystemVersion) throw new Error('App minimum system version does not match the project');
  const main = inside(join(root, 'Contents', 'MacOS', name));
  if (!(statSync(main).mode & 0o111)) throw new Error('macOS app binary has no executable permission');
  const binaries = new Map(), visited = new Set(), systemImports = new Set();
  const inspect = file => { if (!binaries.has(file)) binaries.set(file, inspectMacosBinary(file)); return binaries.get(file); };
  const mainSlices = inspect(main);
  if (!mainSlices.some(s => s.architecture === expectedArchitecture) || mainSlices.some(s => s.filetype !== 2)) throw new Error('macOS executable architecture/type mismatch');
  function expand(value, loader) {
    for (const [token, base] of [['@executable_path', dirname(main)], ['@loader_path', dirname(loader)]]) {
      if (value === token || value.startsWith(`${token}/`)) return resolve(base, value.slice(token.length + 1));
    }
    if (isAbsolute(value)) return value;
    throw new Error(`Unresolvable Mach-O load path: ${value}`);
  }
  function visit(file, architecture, inherited) {
    const key = `${file}:${architecture}`;
    if (visited.has(key)) return;
    visited.add(key);
    const slice = inspect(file).find(s => s.architecture === architecture);
    if (!slice || (file !== main && slice.filetype !== 6)) throw new Error(`Bundled library architecture/type mismatch: ${file}`);
    if (newer(slice.minimum, declaredMinimum)) throw new Error(`App understates binary minimum macOS version: ${file}`);
    const rpaths = [...new Set([...slice.rpaths.map(p => expand(p, file)), ...inherited])];
    for (const dependency of slice.imports) {
      if (/(?:^|[/._-])(?:lib)?(?:java(?!script)|jvm|jni)/i.test(dependency)) throw new Error(`Java/JVM/JNI runtime dependency: ${dependency}`);
      if (systemLibrary(dependency)) { systemImports.add(dependency); continue; }
      const candidates = dependency.startsWith('@rpath/') ? rpaths.map(p => resolve(p, dependency.slice(7))) : [expand(dependency, file)];
      const found = candidates.find(p => existsSync(p));
      if (!found) throw new Error(`Missing macOS app library: ${dependency}`);
      visit(inside(found), architecture, rpaths);
    }
  }
  for (const slice of mainSlices) visit(main, slice.architecture, []);
  return { architectures: mainSlices.map(s => s.architecture), executable: name, minimumSystemVersion: info.LSMinimumSystemVersion, symbolicLinks, bundledLibraries: binaries.size - 1, systemImports: [...systemImports].sort() };
}

// Validate a complete copy before replacing the previous deliverable. Relative
// framework links must remain relative after it leaves the build directory.
export function publishMacosBundle(source, destination, validate) {
  const temporary = mkdtempSync(join(dirname(destination), '.amikvm-app-'));
  const staged = join(temporary, basename(destination)), backup = join(temporary, 'previous.app');
  let moved = false, published = false;
  try {
    cpSync(source, staged, { recursive: true, dereference: false, verbatimSymlinks: true, preserveTimestamps: true });
    const info = validate(staged);
    if (lstatSync(destination, { throwIfNoEntry: false })) { renameSync(destination, backup); moved = true; }
    try { renameSync(staged, destination); published = true; }
    catch (error) {
      if (moved) {
        try { renameSync(backup, destination); moved = false; }
        catch (restore) { throw new AggregateError([error, restore], `Previous app retained at ${backup}; restore it before retrying`); }
      }
      throw error;
    }
    return info;
  } finally {
    if (!moved || published) rmSync(temporary, { recursive: true, force: true });
  }
}
