import { copyFile, mkdir, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const source = path.join(root, 'sandbox', 'wslc');
const destination = path.join(root, 'dist', 'sandbox', 'wslc');

async function copyTree(from, to) {
  await mkdir(to, { recursive: true });
  for (const entry of await readdir(from, { withFileTypes: true })) {
    const sourcePath = path.join(from, entry.name);
    const destinationPath = path.join(to, entry.name);
    if (entry.isDirectory()) await copyTree(sourcePath, destinationPath);
    else if (entry.isFile()) await copyFile(sourcePath, destinationPath);
    else throw new Error(`Unsupported sandbox asset entry: ${sourcePath}`);
  }
}

await copyTree(source, destination);
