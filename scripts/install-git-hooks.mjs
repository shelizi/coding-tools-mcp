import { chmodSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';

function runGit(args, { allowFailure = false } = {}) {
  const result = spawnSync('git', args, { encoding: 'utf8' });
  if (result.error) {
    throw result.error;
  }
  if (!allowFailure && result.status !== 0) {
    if (result.stderr) process.stderr.write(result.stderr);
    throw new Error(`git exited with status ${result.status ?? 'unknown'}`);
  }
  return result;
}

const root = runGit(['rev-parse', '--show-toplevel']).stdout.trim();
const configured = runGit(
  ['-C', root, 'config', '--local', '--get', 'core.hooksPath'],
  { allowFailure: true },
).stdout.trim();

const acceptedHookPaths = new Set(['.githooks', './.githooks']);
if (configured && !acceptedHookPaths.has(configured.replaceAll('\\', '/'))) {
  console.error(
    `hooks:install: refusing to replace existing core.hooksPath=${JSON.stringify(configured)}`,
  );
  console.error('Set it to .githooks yourself, or remove the existing local setting first.');
  process.exit(1);
}

runGit(['-C', root, 'config', '--local', 'core.hooksPath', '.githooks']);

try {
  chmodSync(resolve(root, '.githooks', 'pre-commit'), 0o755);
} catch (error) {
  console.error(`hooks:install: unable to mark pre-commit executable: ${error.message}`);
  process.exit(1);
}

console.log('hooks:install: core.hooksPath=.githooks');
