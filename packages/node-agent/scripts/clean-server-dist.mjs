import { readdir, rm } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const dist = path.join(root, 'dist');

let entries = [];
try {
  entries = await readdir(dist, { withFileTypes: true });
} catch (error) {
  if (error?.code !== 'ENOENT') throw error;
}

await Promise.all(entries
  .filter(entry => entry.name !== 'ui')
  .map(entry => rm(path.join(dist, entry.name), { recursive: true, force: true })));
