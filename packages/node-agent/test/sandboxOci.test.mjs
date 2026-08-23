import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  buildOciMounts,
  buildOciRunArgs,
  prepareOciLaunch,
  selectedOciImage,
  selectedOciNetwork
} from '../dist/sandboxOci.js';

async function directories(t) {
  const base = await mkdtemp(path.join(tmpdir(), 'ctmcp-node-sandbox-oci-'));
  const workspace = path.join(base, 'workspace');
  const writable = path.join(base, 'writable');
  const readonly = path.join(base, 'readonly');
  const nestedReadonly = path.join(writable, 'nested-readonly');
  await Promise.all([workspace, writable, readonly, nestedReadonly].map(value => mkdir(value, { recursive: true })));
  t.after(() => rm(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  return { workspace, writable, readonly, nestedReadonly };
}

test('OCI options default to isolation-first image and network', () => {
  const config = { enabled: true, backend: 'docker', externalPaths: [], options: {} };
  assert.equal(selectedOciImage('docker', config), 'ubuntu:24.04');
  assert.equal(selectedOciNetwork('podman', { ...config, backend: 'podman' }), 'none');
});

test('OCI host networking is rejected as a sandbox escape', () => {
  assert.throws(
    () => selectedOciNetwork('docker', { enabled: true, backend: 'docker', externalPaths: [], options: { 'docker.network': 'host' } }),
    error => error?.code === 'SANDBOX_OCI_NETWORK_FORBIDDEN'
  );
});

test('OCI whitespace image is rejected', () => {
  assert.throws(
    () => selectedOciImage('podman', { enabled: true, backend: 'podman', externalPaths: [], options: { 'podman.image': 'ubuntu alpine' } }),
    error => error?.code === 'SANDBOX_OCI_IMAGE_INVALID'
  );
});

test('OCI mount model preserves read-only grants without broadening the workspace', async t => {
  const { workspace, writable, readonly } = await directories(t);
  const mounts = await buildOciMounts('docker', workspace, [
    { path: readonly, access: 'read_only' },
    { path: writable, access: 'modify' }
  ]);
  assert.equal(mounts[0].host, workspace);
  assert.equal(mounts[0].container, '/workspace');
  assert.equal(mounts[0].access, 'modify');
  assert.ok(mounts.some(mount => mount.host === readonly && mount.access === 'read_only'));
  assert.ok(mounts.some(mount => mount.host === writable && mount.access === 'modify'));
});

test('OCI rejects writable-parent/read-only-child overlaps', async t => {
  const { workspace, writable, nestedReadonly } = await directories(t);
  await assert.rejects(
    buildOciMounts('podman', workspace, [
      { path: writable, access: 'modify' },
      { path: nestedReadonly, access: 'read_only' }
    ]),
    error => error?.code === 'SANDBOX_OCI_MOUNT_OVERLAP'
  );
});

test('OCI file grants fail closed instead of broadening to the parent directory', async t => {
  const { workspace } = await directories(t);
  const file = path.join(path.dirname(workspace), 'single.txt');
  await writeFile(file, 'secret');
  await assert.rejects(
    buildOciMounts('docker', workspace, [{ path: file, access: 'read_only' }]),
    error => error?.code === 'SANDBOX_OCI_PATH_INVALID'
  );
});

test('OCI launch uses an ephemeral container and forwards environment explicitly', async t => {
  const { workspace, readonly } = await directories(t);
  const mounts = await buildOciMounts('docker', workspace, [{ path: readonly, access: 'read_only' }]);
  const launch = prepareOciLaunch(
    { runtime: 'docker', cli: 'docker', image: 'alpine:3.20', network: 'none', mounts },
    workspace,
    { program: 'sh', argv: ['-c', 'printf ok'], display: 'sh -c printf ok', shell: false },
    [['A', 'B'], ['DROP', 'value']],
    ['DROP']
  );
  assert.equal(launch.program, 'docker');
  assert.equal(launch.environmentMode, 'forwarded');
  assert.equal(launch.processTreeContained, true);
  assert.equal(launch.processTreeControl, 'docker_container');
  assert.deepEqual(launch.args.slice(0, 5), ['run', '--rm', '-i', '--name', launch.args[4]]);
  assert.ok(launch.args.includes('--network'));
  assert.ok(launch.args.includes('none'));
  assert.ok(launch.args.includes('--security-opt'));
  assert.ok(launch.args.includes('no-new-privileges'));
  assert.ok(launch.args.includes('A=B'));
  assert.ok(!launch.args.includes('DROP=value'));
  assert.ok(launch.args.includes('env'));
  assert.ok(launch.args.includes('-u'));
  assert.ok(launch.args.includes('DROP'));
  const args = buildOciRunArgs(
    { runtime: 'podman', cli: 'podman', image: 'alpine:3.20', network: 'none', mounts },
    'ctmcp-test',
    workspace,
    { program: 'python', argv: ['--version'], display: 'python --version', shell: false },
    [],
    []
  );
  assert.equal(args[0], 'run');
  assert.equal(args.at(-2), 'python');
  assert.equal(args.at(-1), '--version');
});
