import { readdir, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const forbidden = /\.(exe|dll|node)$/i;
const allowedAppContainerHelper = /^dist\/appcontainer-helper-[0-9a-f]{16}\.exe$/i;
const allowedTypeScriptNativeCompiler = /^node_modules\/@typescript\/typescript-win32-x64\/lib\/tsc\.exe$/i;
async function walk(dir) {
  for (const entry of await readdir(dir, { withFileTypes: true })) {
    if (entry.name === '.git') continue;
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) await walk(full);
    else if (forbidden.test(entry.name)) {
      const relative = path.relative(root, full).split(path.sep).join('/');
      if (!allowedAppContainerHelper.test(relative) && !allowedTypeScriptNativeCompiler.test(relative)) {
        throw new Error(`native binary is forbidden: ${full}`);
      }
    }
  }
}

await walk(root);
const pkg = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
for (const key of ['preinstall', 'install', 'postinstall']) {
  if (pkg.scripts?.[key]) throw new Error(`package lifecycle build/download script is forbidden: ${key}`);
}
const workspaceConfigPath = path.join(root, '..', '..', 'pnpm-workspace.yaml');
const workspaceConfig = await readFile(workspaceConfigPath, 'utf8');
const allowBuildsSection = workspaceConfig.match(/(?:^|\r?\n)allowBuilds:\r?\n((?:[ \t]+[^\r\n]+\r?\n?)*)/);
const allowedBuildPackages = new Set(
  [...(allowBuildsSection?.[1] ?? '').matchAll(/^\s{2}([^:#\s]+):\s*true\s*$/gm)].map((match) => match[1])
);
const unexpectedBuildPackages = [...allowedBuildPackages].filter((name) => name !== 'esbuild');
if (unexpectedBuildPackages.length) {
  throw new Error(`unexpected pnpm build-script allowlist entry: ${unexpectedBuildPackages.join(', ')}`);
}
const allowedProductionDependencies = new Set(['jpeg-js', 'pngjs', 'smol-toml', 'ws']);
const productionDependencies = Object.keys(pkg.dependencies ?? {});
const unexpectedDependencies = productionDependencies.filter(name => !allowedProductionDependencies.has(name));
if (unexpectedDependencies.length) {
  throw new Error(`unexpected production dependency: ${unexpectedDependencies.join(', ')}`);
}
console.log('node-agent contains no forbidden native binaries or lifecycle scripts; pnpm build-script allowlist is restricted; packaged AppContainer helper and TypeScript native compiler are allowed');
