import { spawnSync } from 'node:child_process';
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';
import { validateWindowsExecutable } from './windows-executable.mjs';
import { publishMacosBundle, validateMacosBundle } from './macos-bundle.mjs';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
process.chdir(root);
const temporaryRoot = join(root, '.local-work', 'tmp');
mkdirSync(temporaryRoot, { recursive: true });
Object.assign(process.env, { TMPDIR: temporaryRoot, TMP: temporaryRoot, TEMP: temporaryRoot });
const { values } = parseArgs({ options: {
  target: { type: 'string' },
  runner: { type: 'string' },
  help: { type: 'boolean', short: 'h' },
} });
if (values.help) {
  console.log('Usage: pnpm package [--target <Rust triple or universal-apple-darwin>] [--runner <Cargo runner>]');
  process.exit(0);
}
function run(command, args, options = {}) {
  const result = spawnSync(command, args, { stdio: 'inherit', shell: process.platform === 'win32', ...options });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with ${result.status}`);
}
const host = spawnSync('rustc', ['-vV'], { encoding: 'utf8' });
if (host.status !== 0) throw new Error(host.error?.message ?? host.stderr);
const hostTarget = /^host: (.+)$/m.exec(host.stdout)?.[1];
const buildTarget = values.target ?? process.env.CARGO_BUILD_TARGET ?? hostTarget;
if (!buildTarget || !/^[a-z0-9_-]+$/.test(buildTarget)) throw new Error('Invalid Rust target triple');
const platform = buildTarget.includes('-windows-') ? 'windows'
  : buildTarget.includes('-linux-') ? 'linux'
  : buildTarget.endsWith('-apple-darwin') ? 'macos' : undefined;
const architecture = buildTarget === 'universal-apple-darwin' ? 'universal'
  : { x86_64: 'x64', i686: 'ia32', aarch64: 'arm64' }[buildTarget.split('-')[0]];
if (!platform || !architecture) throw new Error(`Unsupported package target: ${buildTarget}`);
if (platform === 'windows' && !buildTarget.endsWith('-msvc')) {
  throw new Error('Windows single-file packages require an MSVC target; GNU builds depend on WebView2Loader.dll');
}
if (platform === 'macos' && process.platform !== 'darwin') throw new Error('macOS app packaging requires a macOS host');
if (platform === 'linux' && process.platform !== 'linux') throw new Error('Linux packaging requires a Linux host');
const runner = values.runner ?? (platform === 'windows' && process.platform !== 'win32' ? 'cargo-xwin' : undefined);
if (runner && !/^[a-zA-Z0-9_-]+$/.test(runner)) throw new Error('Runner must be an executable name on PATH');
const explicitTarget = Boolean(values.target ?? process.env.CARGO_BUILD_TARGET);
const buildArguments = ['tauri', 'build', ...(explicitTarget ? ['--target', buildTarget] : []), ...(runner ? ['--runner', runner] : [])];
const metadata = spawnSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], { encoding: 'utf8' });
if (metadata.status !== 0) throw new Error(metadata.stderr);
const target = JSON.parse(metadata.stdout).target_directory;
const release = explicitTarget ? join(target, buildTarget, 'release') : join(target, 'release');
const version = JSON.parse(readFileSync('package.json', 'utf8')).version;
const output = join(root, 'artifacts');
mkdirSync(output, { recursive: true });
if (platform === 'macos') {
  run('pnpm', [...buildArguments, '--bundles', 'app']);
  const destination = join(output, 'AMIKVM.app');
  const config = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
  const info = publishMacosBundle(join(release, 'bundle', 'macos', 'AMIKVM.app'), destination, path => validateMacosBundle(path, architecture, {
    identifier: config.identifier, version, minimumSystemVersion: config.bundle.macOS.minimumSystemVersion,
  }));
  console.log(`Created ${destination}; ${info.architectures.join('/')} executable, ${info.bundledLibraries} bundled libraries`);
} else if (platform === 'windows') {
  run('pnpm', [...buildArguments, '--no-bundle']);
  const info = validateWindowsExecutable(join(release, 'amikvm.exe'), architecture);
  const destination = join(output, `AMIKVM-${version}-windows-${architecture}.exe`);
  cpSync(join(release, 'amikvm.exe'), destination);
  console.log(`Created ${destination}; ${info.imports.length} system DLL imports, no companion DLLs`);
} else if (platform === 'linux') {
  run('pnpm', [...buildArguments, '--no-bundle']);
  const temporary = mkdtempSync(join(temporaryRoot, 'amikvm-package-'));
  try {
    const folder = join(temporary, 'AMIKVM');
    mkdirSync(folder);
    cpSync(join(release, 'amikvm'), join(folder, 'AMIKVM'));
    cpSync(join(root, 'vendor', 'fatfs', 'LICENSE.txt'), join(folder, 'LICENSE-fatfs.txt'));
    cpSync(join(root, 'vendor', 'fatfs', 'AMIKVM-CHANGES.md'), join(folder, 'FAT-CHANGES.md'));
    writeFileSync(join(folder, 'README.txt'), 'AMIKVM\n\nRun ./AMIKVM from this directory.\nRequires GTK 3 and WebKitGTK 4.1 provided by your Linux distribution.\n');
    const name = `AMIKVM-${version}-linux-${architecture}.tar.gz`;
    run('tar', ['-czf', join(temporary, name), '-C', temporary, 'AMIKVM']);
    const destination = join(output, name);
    cpSync(join(temporary, name), destination);
    console.log(`Created ${destination}`);
  } finally { rmSync(temporary, { recursive: true, force: true }); }
}
