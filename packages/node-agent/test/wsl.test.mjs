import test from 'node:test';
import assert from 'node:assert/strict';
import { normalizeConfig } from '../dist/config.js';
import { formatterExecutableCandidates, formatterLaunchSpec } from '../dist/formatterTools.js';
import { defaultPolicy, resolveCommandSpec, validateToolPolicy } from '../dist/policy.js';
import { normalizeSecurityPolicy } from '../dist/securityPolicy.js';
import { runBuffered } from '../dist/processes.js';
import { relativeInside, resolveInside, rootAndCwd } from '../dist/workspace.js';
import {
  compareWslPaths, decodeWslOutput, parseWslUncPath,
  validateWslExecPaths, validateWslWorkspacePath, WslRoutingError,
  wslInvocationForPath, wslUncPath
} from '../dist/wsl.js';

const ROOT = String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project`;

function context() {
  return {
    config: normalizeConfig({
      permissionMode: 'trusted',
      toolProfile: 'core',
      policy: defaultPolicy(),
      securityPolicy: normalizeSecurityPolicy(undefined, 'trusted', 'core'),
      folders: [{ id: 'repo', name: 'Repo', path: ROOT }]
    }, {
      oauthPassword: 'wsl-test-password',
      oauthTokenSecret: 'wsl-test-token-secret-that-is-long-enough'
    }, {}),
    selections: new Map([['wsl-test', 'repo']]),
    defaultCwds: new Map()
  };
}

test('WSL UNC parser matches Rust forms, normalization and case semantics', () => {
  for (const value of [
    String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project`,
    String.raw`\\wsl$\Ubuntu-24.04\opt\src\Sample Project`,
    String.raw`\\?\UNC\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project`
  ]) {
    assert.deepEqual(parseWslUncPath(value), {
      distro: 'Ubuntu-24.04',
      linuxPath: '/opt/src/Sample Project'
    });
  }
  assert.equal(
    wslUncPath('Ubuntu-24.04', '/opt//src/./Sample Project/'),
    ROOT
  );
  assert.equal(compareWslPaths(ROOT, String.raw`\\wsl$\ubuntu-24.04\opt\src\Sample Project`), true);
  assert.equal(compareWslPaths(ROOT, String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\sample project`), false);
  assert.equal(compareWslPaths('C:\\src\\Demo', 'c:\\src\\demo'), undefined);
});

test('configuration and workspace containment preserve canonical WSL paths', () => {
  const config = normalizeConfig({
    schema_version: 1,
    folders: [{ id: 'repo', name: '', path: String.raw`\\wsl$\Ubuntu-24.04\opt\\src\.\Sample Project` }]
  }, { oauthPassword: 'password', oauthTokenSecret: 'token' }, {});
  assert.equal(config.folders[0].path, ROOT);
  assert.equal(config.folders[0].name, 'Sample Project');

  const nested = resolveInside(ROOT, 'packages/../src/index.ts');
  assert.equal(nested, String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project\src\index.ts`);
  assert.equal(relativeInside(ROOT, nested), 'src/index.ts');
  assert.throws(() => resolveInside(ROOT, '../outside'), /PATH_OUTSIDE_WORKSPACE/);
  assert.throws(
    () => resolveInside(ROOT, String.raw`\\wsl.localhost\Debian\tmp\other`),
    error => error instanceof WslRoutingError && error.code === 'WSL_CROSS_DISTRIBUTION_PATH'
  );

  const ctx = context();
  ctx.defaultCwds.set('wsl-test', 'src');
  assert.deepEqual(rootAndCwd(ctx, 'wsl-test'), {
    folder: ctx.config.folders[0],
    root: ROOT,
    cwd: String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project\src`
  });
});

test('WSL invocation is shell-free and translates cwd, paths and environment exactly', () => {
  const invocation = wslInvocationForPath(
    ROOT,
    'cargo',
    [
      'test',
      String.raw`\\wsl$\ubuntu-24.04\opt\src\Sample Project\Cargo.toml`
    ],
    [['RUST_LOG', 'debug trace']],
    ['RUST_BACKTRACE'],
    'win32'
  );
  assert.deepEqual(invocation, {
    program: 'wsl.exe',
    args: [
      '--distribution', 'Ubuntu-24.04',
      '--cd', '/opt/src/Sample Project',
      '--exec', 'env', '-u', 'RUST_BACKTRACE', 'RUST_LOG=debug trace',
      'cargo', 'test', '/opt/src/Sample Project/Cargo.toml'
    ]
  });
  assert.throws(
    () => validateWslExecPaths(ROOT, 'cargo', [String.raw`\\wsl.localhost\Debian\tmp\Cargo.toml`]),
    error => error instanceof WslRoutingError
      && error.code === 'WSL_CROSS_DISTRIBUTION_PATH'
      && error.details.position === 'args[0]'
  );
  assert.throws(
    () => validateWslExecPaths(ROOT, 'cargo', [String.raw`C:\src\Cargo.toml`]),
    error => error instanceof WslRoutingError
      && error.code === 'WSL_HOST_PATH_REQUIRES_TRANSLATION'
      && error.retryable === true
  );
});

test('WSL output decoding and workspace validation are testable without WSL', async () => {
  const utf16 = Buffer.from('\uFEFFdistribution missing\r\n', 'utf16le');
  assert.equal(decodeWslOutput(utf16).trim(), 'distribution missing');

  const calls = [];
  await validateWslWorkspacePath(ROOT, async (program, args) => {
    calls.push({ program, args });
    return { code: 0, stdout: Buffer.alloc(0), stderr: Buffer.alloc(0) };
  }, 'win32');
  assert.deepEqual(calls, [{
    program: 'wsl.exe',
    args: [
      '--distribution', 'Ubuntu-24.04',
      '--cd', '/opt/src/Sample Project',
      '--exec', 'test', '-d', '.'
    ]
  }]);

  await assert.rejects(
    validateWslWorkspacePath(
      String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\..\outside`,
      async () => ({ code: 0, stdout: Buffer.alloc(0), stderr: Buffer.alloc(0) }),
      'win32'
    ),
    error => error instanceof WslRoutingError && error.code === 'WSL_WORKSPACE_PATH_INVALID'
  );
  await assert.rejects(
    validateWslWorkspacePath(ROOT, async () => ({
      code: 1,
      stdout: Buffer.alloc(0),
      stderr: Buffer.from('distribution missing', 'utf8')
    }), 'win32'),
    error => error instanceof WslRoutingError
      && error.code === 'WSL_WORKSPACE_UNAVAILABLE'
      && error.details.distro === 'Ubuntu-24.04'
  );
  await assert.rejects(
    validateWslWorkspacePath(ROOT, async () => ({ code: 0, stdout: Buffer.alloc(0), stderr: Buffer.alloc(0) }), 'linux'),
    error => error instanceof WslRoutingError && error.code === 'WSL_UNSUPPORTED_PLATFORM'
  );
});

test('WSL command policy uses distro PATH, Linux absolute programs and sh only', async () => {
  const ctx = context();
  const key = 'wsl-test';
  await validateToolPolicy(ctx, key, 'exec_command', { program: 'cargo', args: ['test'] });
  assert.deepEqual(await resolveCommandSpec(ctx, key, { program: 'cargo', args: ['test'] }), {
    program: 'cargo', argv: ['test'], display: 'cargo test', shell: false
  });
  assert.deepEqual(await resolveCommandSpec(ctx, key, { program: '/usr/bin/cargo', args: ['check'] }), {
    program: '/usr/bin/cargo', argv: ['check'], display: '/usr/bin/cargo check', shell: false
  });
  ctx.config.policy = { ...ctx.config.policy, workspaceScriptExtensions: ['.sh'] };
  await assert.rejects(
    validateToolPolicy(ctx, key, 'exec_command', { program: '/tmp/outside.sh', args: [] }),
    error => error?.code === 'COMMAND_REJECTED'
  );
  assert.deepEqual(await resolveCommandSpec(ctx, key, { script: 'printf ok', shell: 'sh', confirm: true }), {
    program: 'sh', argv: ['-c', 'printf ok'], display: 'printf ok', shell: false
  });
  await assert.rejects(
    validateToolPolicy(ctx, key, 'exec_command', { script: 'echo ok', shell: 'cmd', confirm: true }),
    error => error?.code === 'INVALID_ARGUMENT'
  );
  await assert.rejects(
    validateToolPolicy(ctx, key, 'exec_command', {
      program: 'cargo', args: [String.raw`\\wsl.localhost\Debian\tmp\Cargo.toml`]
    }),
    error => error?.code === 'WSL_CROSS_DISTRIBUTION_PATH'
  );
});

test('formatter discovery and custom JavaScript launch stay inside the selected distribution', () => {
  const candidates = formatterExecutableCandidates(ROOT, ['prettier']);
  assert.ok(candidates.includes(String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project\node_modules\.bin\prettier`));
  assert.ok(candidates.includes(String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project\.venv\bin\prettier`));
  assert.ok(!candidates.some(candidate => candidate.endsWith('.cmd')));

  const executable = resolveInside(ROOT, 'tools/formatter.cjs');
  const mirror = resolveInside(ROOT, '.coding-tools-format/run-1');
  const launch = formatterLaunchSpec(ROOT, executable, ['src/example.ts']);
  assert.deepEqual(launch, { program: 'node', args: [executable, 'src/example.ts'] });
  assert.deepEqual(
    wslInvocationForPath(mirror, launch.program, launch.args, [['NO_COLOR', '1']], [], 'win32'),
    {
      program: 'wsl.exe',
      args: [
        '--distribution', 'Ubuntu-24.04',
        '--cd', '/opt/src/Sample Project/.coding-tools-format/run-1',
        '--exec', 'env', 'NO_COLOR=1',
        'node', '/opt/src/Sample Project/tools/formatter.cjs', 'src/example.ts'
      ]
    }
  );
});

test('live WSL routing when explicitly enabled', {
  skip: process.platform !== 'win32' || !process.env.CTMCP_TEST_WSL_DISTRO
}, async () => {
  const distro = process.env.CTMCP_TEST_WSL_DISTRO;
  const linuxPath = process.env.CTMCP_TEST_WSL_PATH || '/tmp';
  const cwd = wslUncPath(distro, linuxPath);
  await validateWslWorkspacePath(cwd);
  const result = await runBuffered('sh', ['-c', 'pwd'], cwd, undefined, 30_000, process.env, { routeWsl: true });
  assert.equal(result.code, 0, result.stderr);
  assert.equal(result.stdout.trim(), linuxPath.replace(/\/$/, '') || '/');
});
