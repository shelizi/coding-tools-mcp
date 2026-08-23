import { execFileSync } from 'node:child_process';
import { mkdir, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const packageRoot = path.dirname(fileURLToPath(new URL('../package.json', import.meta.url)));
const repositoryRoot = path.resolve(packageRoot, '../..');
const environmentSha = String(process.env.CTMCP_BUILD_GIT_SHA ?? '').trim().toLowerCase();
const environmentClean = String(process.env.CTMCP_BUILD_SOURCE_CLEAN ?? '').trim().toLowerCase();

let buildGitSha = /^[0-9a-f]{40}$/.test(environmentSha) ? environmentSha : 'unknown';
let sourceClean = environmentClean === 'true' || environmentClean === '1'
  ? true
  : environmentClean === 'false' || environmentClean === '0'
    ? false
    : null;
try {
  if (buildGitSha === 'unknown') {
    const resolved = execFileSync('git', ['-C', repositoryRoot, 'rev-parse', 'HEAD'], {
      encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore']
    }).trim().toLowerCase();
    if (/^[0-9a-f]{40}$/.test(resolved)) buildGitSha = resolved;
  }
  if (sourceClean === null) {
    const status = execFileSync('git', ['-C', repositoryRoot, 'status', '--porcelain', '--untracked-files=normal'], {
      encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore']
    });
    sourceClean = status.trim().length === 0;
  }
} catch {
  // Builds without Git remain supported, but runtime trust stays unknown unless provenance was injected.
}

const target = path.join(packageRoot, 'dist', 'build-info.json');
await mkdir(path.dirname(target), { recursive: true });
await writeFile(target, `${JSON.stringify({ schemaVersion: 1, buildGitSha, sourceClean }, null, 2)}\n`, 'utf8');
