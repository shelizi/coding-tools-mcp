import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import path from 'node:path';
import test from 'node:test';

import {
  acquireWslcStorageProcessLock,
  windowsPowerShellPath,
  wslcStorageMutexName,
  wslcStorageProcessLockHostAvailable
} from '../dist/sandboxWslcProcessLock.js';

const windowsOnly = { skip: process.platform !== 'win32' };

test('WSLC storage mutex identity is stable and path-specific', () => {
  const left = wslcStorageMutexName('C:\\sandbox\\workspace-a');
  assert.equal(left, wslcStorageMutexName('C:\\sandbox\\workspace-a\\'));
  assert.notEqual(left, wslcStorageMutexName('C:\\sandbox\\workspace-b'));
  assert.match(left, /^Local\\CodingToolsMCP\.WslcStorage\.[0-9a-f]{64}$/);
});

test('WSLC storage process lock times out and then succeeds after release', windowsOnly, async () => {
  assert.equal(wslcStorageProcessLockHostAvailable(), true);
  assert.ok(windowsPowerShellPath().toLowerCase().endsWith('windowspowershell\\v1.0\\powershell.exe'));
  const storage = path.join(process.cwd(), `.wslc-process-lock-${process.pid}-timeout`);
  const first = await acquireWslcStorageProcessLock(storage, undefined, 5_000);
  try {
    await assert.rejects(
      acquireWslcStorageProcessLock(storage, undefined, 200),
      error => error?.code === 'SANDBOX_WSLC_PROCESS_LOCK_TIMEOUT' && error?.retryable === true
    );
  } finally {
    await first.release();
  }
  const second = await acquireWslcStorageProcessLock(storage, undefined, 5_000);
  await second.release();
});

test('WSLC storage process lock wait is abortable', windowsOnly, async () => {
  const storage = path.join(process.cwd(), `.wslc-process-lock-${process.pid}-abort`);
  const first = await acquireWslcStorageProcessLock(storage, undefined, 5_000);
  const controller = new AbortController();
  try {
    const waiting = acquireWslcStorageProcessLock(storage, controller.signal, 5_000);
    setTimeout(() => controller.abort(), 150).unref();
    await assert.rejects(
      waiting,
      error => error?.code === 'SANDBOX_WSLC_QUEUE_CANCELLED'
    );
  } finally {
    await first.release();
  }
});

test('WSLC storage process lock recovers when the owning Node process is killed', windowsOnly, async t => {
  const storage = path.join(process.cwd(), `.wslc-process-lock-${process.pid}-owner-crash`);
  const moduleUrl = new URL('../dist/sandboxWslcProcessLock.js', import.meta.url).href;
  const worker = String.raw`
const { acquireWslcStorageProcessLock } = await import(process.argv[1]);
const lock = await acquireWslcStorageProcessLock(process.argv[2], undefined, 5000);
console.log('worker-acquired');
await new Promise(() => {});
void lock;
`;
  const child = spawn(process.execPath, ['--input-type=module', '-e', worker, moduleUrl, storage], {
    cwd: process.cwd(),
    windowsHide: true,
    stdio: ['ignore', 'pipe', 'pipe']
  });
  t.after(() => {
    if (child.exitCode === null && child.signalCode === null) child.kill('SIGKILL');
  });
  let stdout = '';
  child.stdout.on('data', chunk => { stdout += Buffer.from(chunk).toString('utf8'); });
  const readyDeadline = Date.now() + 10_000;
  while (!stdout.includes('worker-acquired') && Date.now() < readyDeadline) {
    if (child.exitCode !== null || child.signalCode !== null) break;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  assert.match(stdout, /worker-acquired/);
  await assert.rejects(
    acquireWslcStorageProcessLock(storage, undefined, 200),
    error => error?.code === 'SANDBOX_WSLC_PROCESS_LOCK_TIMEOUT'
  );

  child.kill('SIGKILL');
  if (child.exitCode === null && child.signalCode === null) await once(child, 'exit');
  const recovered = await acquireWslcStorageProcessLock(storage, undefined, 5_000);
  await recovered.release();
});
