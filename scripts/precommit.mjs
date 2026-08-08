import { spawnSync } from 'node:child_process';

function run(program, args, options = {}) {
  const result = spawnSync(program, args, {
    cwd: options.cwd,
    encoding: options.encoding ?? 'utf8',
    stdio: options.stdio ?? 'pipe',
  });

  if (result.error) {
    const detail = result.error.code === 'ENOENT'
      ? `${program} was not found on PATH`
      : result.error.message;
    throw new Error(detail);
  }

  if (!options.allowFailure && result.status !== 0) {
    if (result.stdout) process.stdout.write(result.stdout);
    if (result.stderr) process.stderr.write(result.stderr);
    throw new Error(`${program} exited with status ${result.status ?? 'unknown'}`);
  }

  return result;
}

function git(root, args, options = {}) {
  return run('git', ['-C', root, ...args], options);
}

function nulSeparatedPaths(root, args) {
  const result = git(root, [...args, '-z', '--', '*.rs'], { encoding: 'buffer' });
  return result.stdout
    .toString('utf8')
    .split('\0')
    .filter(Boolean);
}

function fail(message, details = []) {
  console.error(`pre-commit: ${message}`);
  for (const detail of details) console.error(`  ${detail}`);
  process.exit(1);
}

let root;
try {
  root = run('git', ['rev-parse', '--show-toplevel']).stdout.trim();
} catch (error) {
  fail(error.message);
}

let stagedRustFiles;
let unstagedRustFiles;
try {
  stagedRustFiles = nulSeparatedPaths(root, [
    'diff',
    '--cached',
    '--name-only',
    '--diff-filter=ACMR',
  ]);

  if (stagedRustFiles.length === 0) {
    process.exit(0);
  }

  unstagedRustFiles = new Set(nulSeparatedPaths(root, [
    'diff',
    '--name-only',
    '--diff-filter=ACMR',
  ]));
} catch (error) {
  fail(error.message);
}

const partiallyStaged = stagedRustFiles.filter((file) => unstagedRustFiles.has(file));
if (partiallyStaged.length > 0) {
  fail(
    'Rust formatting was skipped because these files are partially staged.',
    [
      ...partiallyStaged,
      'Format the files, then re-stage only the hunks you intend to commit.',
    ],
  );
}

console.log(`pre-commit: rustfmt ${stagedRustFiles.length} staged Rust file(s)`);

try {
  run(
    'rustfmt',
    [
      '--edition',
      '2021',
      '--config',
      'skip_children=true',
      ...stagedRustFiles,
    ],
    { cwd: root, stdio: 'inherit' },
  );

  git(root, ['add', '--', ...stagedRustFiles], { stdio: 'inherit' });
} catch (error) {
  fail(error.message);
}
