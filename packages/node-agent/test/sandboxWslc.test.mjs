import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  buildWslcMounts,
  buildWslcRunArgs,
  configuredWslcSessionStorage,
  containerPathForHost,
  prepareWslc,
  prepareWslcLaunch,
  selectedWslcImage,
  selectedWslcNetwork,
  WSLC_DEFAULT_IMAGE
} from '../dist/sandboxWslc.js';
import {
  ensureWslcSessionStorage,
  managedWslcSessionStorage,
  wslcProvisionerCompilerCandidates
} from '../dist/sandboxWslcProvisioner.js';

async function directories(t) {
  const base = await mkdtemp(path.join(tmpdir(), 'ctmcp-node-wslc-unit-'));
  const workspace = path.join(base, 'workspace');
  const writable = path.join(base, 'writable');
  const readonly = path.join(base, 'readonly');
  const nestedReadonly = path.join(writable, 'nested-readonly');
  await Promise.all([workspace, writable, readonly, nestedReadonly].map(value => mkdir(value, { recursive: true })));
  t.after(() => rm(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return { workspace, writable, readonly, nestedReadonly };
}

test('WSLC option defaults are explicit and network values are bounded', () => {
  const config = { enabled: true, backend: 'wslc', externalPaths: [], options: {} };
  assert.equal(selectedWslcImage(config), 'coding-tools-mcp/wslc-sandbox:alpine-3.21');
  assert.equal(selectedWslcNetwork(config), 'none');
  assert.throws(
    () => selectedWslcNetwork({ ...config, options: { 'wslc.network': 'bridge;host' } }),
    error => error?.code === 'SANDBOX_WSLC_NETWORK_INVALID'
  );
  assert.equal(configuredWslcSessionStorage(config), undefined);
  assert.equal(
    configuredWslcSessionStorage({ ...config, options: { 'wslc.session_storage': 'C:\\sandbox-session' } }),
    'C:\\sandbox-session'
  );
});

test('WSLC managed storage is stable per canonical workspace and outside the workspace tree', async t => {
  const { workspace, writable } = await directories(t);
  const dataDir = path.join(path.dirname(workspace), 'agent-data');
  await mkdir(dataDir, { recursive: true });
  const first = await managedWslcSessionStorage(dataDir, workspace);
  const repeated = await managedWslcSessionStorage(dataDir, workspace);
  const second = await managedWslcSessionStorage(dataDir, writable);
  assert.equal(first, repeated);
  assert.notEqual(first, second);
  assert.ok(first.startsWith(path.join(dataDir, 'sandbox', 'wslc', 'sessions')));
  assert.ok(!first.startsWith(`${workspace}${path.sep}`));
  assert.match(path.basename(first), /^[0-9a-f]{32}$/);
});

test('WSLC provisioner compiler candidates are fixed Windows framework locations', () => {
  assert.deepEqual(
    wslcProvisionerCompilerCandidates('C:\\Windows'),
    [
      path.join('C:\\Windows', 'Microsoft.NET', 'Framework64', 'v4.0.30319', 'csc.exe'),
      path.join('C:\\Windows', 'Microsoft.NET', 'Framework', 'v4.0.30319', 'csc.exe')
    ]
  );
});

test('WSLC session storage validation fails closed for an ordinary directory', async t => {
  const { workspace } = await directories(t);
  const ordinary = path.join(path.dirname(workspace), 'ordinary-storage');
  const dataDir = path.join(path.dirname(workspace), 'agent-data');
  await Promise.all([ordinary, dataDir].map(value => mkdir(value, { recursive: true })));
  await assert.rejects(
    ensureWslcSessionStorage(ordinary, dataDir, workspace),
    error => error?.code === 'SANDBOX_WSLC_SESSION_STORAGE_INVALID'
  );
});

test('WSLC mount model preserves read-only grants without broadening the workspace', async t => {
  const { workspace, writable, readonly } = await directories(t);
  const mounts = await buildWslcMounts(workspace, [
    { path: readonly, access: 'read_only' },
    { path: writable, access: 'modify' }
  ]);
  assert.equal(mounts[0].container, '/workspace');
  assert.equal(mounts[0].access, 'modify');
  assert.ok(mounts.some(mount => mount.access === 'read_only' && mount.host === readonly));
  assert.ok(mounts.some(mount => mount.access === 'modify' && mount.host === writable));
  assert.equal(containerPathForHost(mounts, path.join(workspace, 'nested')), '/workspace/nested');
  assert.equal(containerPathForHost(mounts, path.join(path.dirname(workspace), 'ungranted')), undefined);
});

test('WSLC rejects writable-parent/read-only-child overlaps', async t => {
  const { workspace, writable, nestedReadonly } = await directories(t);
  await assert.rejects(
    buildWslcMounts(workspace, [
      { path: writable, access: 'modify' },
      { path: nestedReadonly, access: 'read_only' }
    ]),
    error => error?.code === 'SANDBOX_WSLC_MOUNT_OVERLAP'
  );
});

test('WSLC run args use one ephemeral container with explicit mounts, cwd, env and removals', async t => {
  const { workspace, readonly } = await directories(t);
  const mounts = await buildWslcMounts(workspace, [{ path: readonly, access: 'read_only' }]);
  const prepared = { cli: 'wslc', image: 'alpine:3.20', network: 'none', mounts };
  const args = buildWslcRunArgs(
    prepared,
    'ctmcp-test',
    workspace,
    { program: 'sh', argv: ['-c', 'printf ok'], display: 'sh -c printf ok', shell: false },
    [['A', 'B'], ['DROP', 'value']],
    ['DROP']
  );
  assert.deepEqual(args.slice(0, 7), ['run', '--rm', '-i', '--name', 'ctmcp-test', '--network', 'none']);
  assert.ok(args.includes('-v'));
  assert.ok(args.some(value => value.endsWith(':/workspace')));
  assert.ok(args.some(value => value.endsWith(':ro')));
  assert.ok(args.includes('/workspace'));
  assert.ok(args.includes('A=B'));
  assert.ok(!args.includes('DROP=value'));
  assert.ok(args.includes('env'));
  assert.ok(args.includes('-u'));
  assert.equal(args.at(-3), 'sh');
  assert.deepEqual(args.slice(-2), ['-c', 'printf ok']);
});

test('WSLC runtime smoke uses the default image and mounts the repository workspace', async t => {
  if (process.env.CTMCP_WSLC_SMOKE !== '1') {
    t.skip('set CTMCP_WSLC_SMOKE=1 via pnpm test:wslc:smoke to run the integration smoke test');
    return;
  }
  if (process.platform !== 'win32') {
    t.skip('Microsoft WSLC is available only on Windows hosts');
    return;
  }

  const packageRoot = path.resolve(import.meta.dirname, '..');
  const workspaceRoot = path.resolve(packageRoot, '..', '..');
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-wslc-smoke-'));
  const config = {
    enabled: true,
    backend: 'wslc',
    externalPaths: [],
    options: { 'wslc.network': 'none' }
  };
  let launch;
  t.after(async () => {
    if (launch) await launch.cleanup();
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const prepared = await prepareWslc(config, workspaceRoot, dataDir, undefined, 120_000);
  assert.equal(prepared.image, WSLC_DEFAULT_IMAGE);
  launch = prepareWslcLaunch(
    prepared,
    workspaceRoot,
    {
      program: 'sh',
      argv: [
        '-lc',
        [
          'set -eu',
          'test -f package.json',
          'node -e "const major=Number(process.versions.node.split(\'.\')[0]); if (major < 22) process.exit(1)"',
          'python --version',
          'git --version',
          'cc --version | head -n1',
          'rustc --version',
          'go version',
          'printf "wslc-smoke=ok\\n"'
        ].join('; ')
      ],
      display: 'WSLC runtime smoke test',
      shell: false
    },
    [],
    []
  );

  const result = await new Promise((resolve, reject) => {
    const child = spawn(launch.program, launch.args, { stdio: 'pipe', windowsHide: true });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk; });
    child.stderr.on('data', chunk => { stderr += chunk; });
    child.once('error', reject);
    child.once('close', code => resolve({ code, stdout, stderr }));
    child.stdin.end();
  });

  assert.equal(result.code, 0, result.stderr || result.stdout);
  assert.match(result.stdout, /wslc-smoke=ok/);
});
