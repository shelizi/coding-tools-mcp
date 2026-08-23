import { createHash } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { access, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

if (process.platform !== 'win32') process.exit(0);
const helperRequired = process.env.CTMCP_REQUIRE_APPCONTAINER_HELPER === '1';
const helperSkipped = process.env.CTMCP_SKIP_APPCONTAINER_HELPER === '1';
if (helperSkipped) {
  if (helperRequired) throw new Error('AppContainer helper precompile cannot be both required and skipped.');
  console.warn('Skipping optional AppContainer helper precompile; runtime fallback remains available.');
  process.exit(0);
}

const packageRoot = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const { APPCONTAINER_HELPER_SOURCE, APPCONTAINER_HELPER_SOURCE_HASH } = await import('../dist/sandboxAppContainer.js');
const { wslcProvisionerCompilerCandidates } = await import('../dist/sandboxWslcProvisioner.js');

async function exists(value) {
  try {
    await access(value);
    return true;
  } catch {
    return false;
  }
}

let compiler;
for (const candidate of wslcProvisionerCompilerCandidates()) {
  if (await exists(candidate)) {
    compiler = candidate;
    break;
  }
}

if (!compiler) {
  const message = 'Windows .NET Framework csc.exe was not found; AppContainer helper precompile is unavailable.';
  if (helperRequired) throw new Error(message);
  console.warn(`${message} Runtime fallback remains available.`);
  process.exit(0);
}

const target = path.join(packageRoot, 'dist', `appcontainer-helper-${APPCONTAINER_HELPER_SOURCE_HASH}.exe`);
const digestFile = `${target}.sha256`;
const temporaryRoot = await mkdtemp(path.join(packageRoot, '.ctmcp-appcontainer-build-'));
const sourcePath = path.join(temporaryRoot, 'AppContainerHost.cs');
try {
  await writeFile(sourcePath, APPCONTAINER_HELPER_SOURCE, 'utf8');
  try {
    execFileSync(compiler, [
      '/nologo',
      '/optimize+',
      '/target:exe',
      '/platform:anycpu',
      '/r:System.dll',
      '/r:System.Core.dll',
      '/r:System.Security.dll',
      `/out:${target}`,
      sourcePath
    ], {
      cwd: temporaryRoot,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe'],
      timeout: 10_000
    });
    await access(target);
    const digest = createHash('sha256').update(await readFile(target)).digest('hex');
    await writeFile(digestFile, `${digest}\n`, 'utf8');
    console.log(`Precompiled AppContainer helper: ${path.basename(target)}`);
  } catch (error) {
    const message = `AppContainer helper precompile was blocked or failed: ${error?.message ?? error}`;
    if (helperRequired) throw new Error(message, { cause: error });
    console.warn(`${message} Runtime fallback remains available.`);
  }
} finally {
  await rm(temporaryRoot, { recursive: true, force: true });
}
