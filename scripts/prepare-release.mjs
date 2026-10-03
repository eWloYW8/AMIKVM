import { appendFileSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const { values, positionals } = parseArgs({
  options: { check: { type: 'boolean' } }, allowPositionals: true,
});
const tag = positionals[0] ?? process.env.GITHUB_REF_NAME;
const match = /^v?((0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?)$/.exec(tag ?? '');
if (!match || positionals.length > 1 || match[5]?.split('.').some(part => /^0\d+$/.test(part))) {
  throw new Error('Release tag must be a semantic version, for example v0.1.0 or v0.2.0-rc.1');
}
const version = match[1];
const prerelease = Boolean(match[5]);
if (!values.check) {
  const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
  for (const file of ['package.json', 'src-tauri/tauri.conf.json']) {
    const path = join(root, file);
    const document = JSON.parse(readFileSync(path, 'utf8'));
    document.version = version;
    writeFileSync(path, `${JSON.stringify(document, null, 2)}\n`);
  }
  const manifest = join(root, 'Cargo.toml');
  const original = readFileSync(manifest, 'utf8');
  const updated = original.replace(/(\[workspace\.package\]\s*\nversion\s*=\s*)"[^"]+"/, `$1"${version}"`);
  if (updated === original && !original.includes(`version = "${version}"`)) {
    throw new Error('Cannot find the workspace package version');
  }
  writeFileSync(manifest, updated);
  const lock = join(root, 'Cargo.lock');
  let packages = 0;
  const contents = readFileSync(lock, 'utf8').replace(
    /(\[\[package\]\]\s*\nname = "(?:amikvm|amikvm-core)"\s*\nversion = )"[^"]+"/g,
    (_, prefix) => { packages++; return `${prefix}"${version}"`; },
  );
  if (packages !== 2) throw new Error('Cannot find both AMIKVM packages in Cargo.lock');
  writeFileSync(lock, contents);
}
if (process.env.GITHUB_OUTPUT) {
  appendFileSync(process.env.GITHUB_OUTPUT, `version=${version}\nprerelease=${prerelease}\n`);
}
console.log(`Release ${tag}: version ${version}, prerelease ${prerelease}`);
