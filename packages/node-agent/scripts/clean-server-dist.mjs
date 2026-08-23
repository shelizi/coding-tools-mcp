import { cp, mkdir, readdir, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const dist = path.join(root, 'dist');
const ui = path.join(dist, 'ui');
const managementStatic = path.join(root, 'management-static');

let entries = [];
try {
  entries = await readdir(dist, { withFileTypes: true });
} catch (error) {
  if (error?.code !== 'ENOENT') throw error;
}

await Promise.all(entries
  .filter(entry => entry.name !== 'ui')
  .map(entry => rm(path.join(dist, entry.name), { recursive: true, force: true, maxRetries: 20, retryDelay: 100 })));

await mkdir(ui, { recursive: true });
await cp(managementStatic, ui, { recursive: true, force: true });
