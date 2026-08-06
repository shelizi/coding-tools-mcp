import { readdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const forbidden = /\.(exe|dll|node)$/i;
async function walk(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.name === '.git') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) await walk(full);
    else if (forbidden.test(entry.name)) throw new Error(`native binary is forbidden: ${full}`);
  }
}

await walk(root);
const pkg = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
for (const key of ['preinstall', 'install', 'postinstall']) {
  if (pkg.scripts?.[key]) throw new Error(`package lifecycle build/download script is forbidden: ${key}`);
}
const lock = JSON.parse(await readFile(path.join(root, 'package-lock.json'), 'utf8'));
for (const [location, metadata] of Object.entries(lock.packages ?? {})) {
  if (metadata?.hasInstallScript) throw new Error(`dependency install script is forbidden: ${location || '<root>'}`);
}
const allowedProductionDependencies = new Set(['jpeg-js', 'pngjs', 'ws']);
const productionDependencies = Object.keys(pkg.dependencies ?? {});
const unexpectedDependencies = productionDependencies.filter(name => !allowedProductionDependencies.has(name));
if (unexpectedDependencies.length) {
  throw new Error(`unexpected production dependency: ${unexpectedDependencies.join(', ')}`);
}
console.log('node-agent contains no forbidden native binaries or install scripts');
