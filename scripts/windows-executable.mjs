import { readFileSync } from 'node:fs';

// Inspect the file itself on every packaging host. Merely copying an EXE can
// accidentally omit the WebView2 loader, C++ runtime or another companion DLL.
const systemLibraries = new Set([
  'advapi32.dll', 'bcrypt.dll', 'bcryptprimitives.dll', 'comctl32.dll',
  'combase.dll', 'comdlg32.dll', 'crypt32.dll', 'd3d11.dll', 'dcomp.dll', 'dwmapi.dll',
  'dxgi.dll', 'gdi32.dll', 'imm32.dll', 'iphlpapi.dll', 'kernel32.dll', 'msimg32.dll',
  'msvcrt.dll', 'ncrypt.dll', 'ntdll.dll', 'ole32.dll', 'oleaut32.dll',
  'opengl32.dll', 'propsys.dll', 'secur32.dll', 'setupapi.dll', 'shell32.dll',
  'shlwapi.dll', 'ucrtbase.dll', 'user32.dll', 'userenv.dll', 'uxtheme.dll',
  'version.dll', 'winmm.dll', 'winspool.drv', 'ws2_32.dll', 'wtsapi32.dll',
]);

export function validateWindowsExecutable(path, expectedArchitecture) {
  const data = readFileSync(path);
  function requireRange(offset, length) {
    if (!Number.isSafeInteger(offset) || offset < 0 || offset + length > data.length) {
      throw new Error(`Invalid PE range in ${path}`);
    }
  }
  function u16(offset) { requireRange(offset, 2); return data.readUInt16LE(offset); }
  function u32(offset) { requireRange(offset, 4); return data.readUInt32LE(offset); }
  if (u16(0) !== 0x5a4d) throw new Error('Windows package is missing the DOS/PE header');
  const header = u32(0x3c);
  if (u32(header) !== 0x4550) throw new Error('Windows package is missing the PE signature');
  const architecture = { 0x8664: 'x64', 0x14c: 'ia32', 0xaa64: 'arm64' }[u16(header + 4)];
  if (architecture !== expectedArchitecture) throw new Error(`PE architecture mismatch: ${architecture} != ${expectedArchitecture}`);
  if (!(u16(header + 22) & 2) || (u16(header + 22) & 0x2000)) throw new Error('Windows package must be an executable, not a DLL');
  const optional = header + 24;
  const optionalSize = u16(header + 20);
  requireRange(optional, optionalSize);
  const magic = u16(optional);
  if (magic !== 0x10b && magic !== 0x20b) throw new Error('Unsupported PE optional header');
  if (u16(optional + 68) !== 2) throw new Error('Windows package must use the GUI subsystem');
  const sections = [];
  for (let index = 0; index < u16(header + 6); index++) {
    const section = optional + optionalSize + index * 40;
    requireRange(section, 40);
    sections.push({ rva: u32(section + 12), size: u32(section + 16), offset: u32(section + 20) });
  }
  function offsetOf(rva, length) {
    for (const section of sections) {
      const delta = rva - section.rva;
      if (delta >= 0 && delta + length <= section.size) {
        requireRange(section.offset + delta, length);
        return section.offset + delta;
      }
    }
    throw new Error(`Unmapped PE RVA: ${rva}`);
  }
  function libraryName(rva) {
    const offset = offsetOf(rva, 1);
    const end = data.indexOf(0, offset);
    if (end < offset || end - offset > 255) throw new Error('Invalid PE import name');
    offsetOf(rva, end - offset + 1);
    const name = data.toString('ascii', offset, end).toLowerCase();
    if (!/^[a-z0-9_.-]+$/.test(name)) throw new Error('Invalid PE library name');
    return name;
  }
  const directory = optional + (magic === 0x20b ? 112 : 96);
  const directoryCount = u32(directory - 4);
  const imports = new Set();
  // Inspect both eagerly loaded and delay-loaded imports. Delay descriptors
  // without RVA addressing are deliberately rejected rather than skipped.
  for (const [index, size, nameOffset] of [[1, 20, 12], [13, 32, 4]]) {
    if (index >= directoryCount) continue;
    if (directory + (index + 1) * 8 > optional + optionalSize) throw new Error('Invalid PE directory table');
    const rva = u32(directory + index * 8);
    const bytes = u32(directory + index * 8 + 4);
    if (!rva && !bytes) continue;
    if (!rva || bytes < size) throw new Error('Invalid PE import directory');
    let terminated = false;
    for (let used = 0; used + size <= bytes; used += size) {
      const descriptor = offsetOf(rva + used, size);
      if (data.subarray(descriptor, descriptor + size).every(byte => byte === 0)) { terminated = true; break; }
      if (index === 13 && u32(descriptor) !== 1) throw new Error('Unsupported PE delay-import addressing');
      imports.add(libraryName(u32(descriptor + nameOffset)));
    }
    if (!terminated) throw new Error('Unterminated PE import directory');
  }
  if (!imports.size) throw new Error('Windows package has no verifiable system imports');
  const dependencies = [...imports].sort();
  const companion = dependencies.filter(name => !systemLibraries.has(name) && !/^(api|ext)-ms-[a-z0-9-]+\.dll$/.test(name));
  if (companion.length) throw new Error(`Windows single-file package requires companion DLLs: ${companion.join(', ')}`);
  return { architecture, imports: dependencies };
}
