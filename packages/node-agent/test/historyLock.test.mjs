import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, utimes, writeFile } from 'node:fs/promises';
import { hostname, tmpdir } from 'node:os';
import path from 'node:path';

import { HistoryError } from '../dist/historyModel.js';
import { acquireHistoryLock, HISTORY_LOCK_DIR } from '../dist/historyStorage.js';

async function fixture(t) {
  const dir = await mkdtemp(path.join(tmpdir(), 'ctmcp-history-lock-'));
  t.after(() => rm(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 20 }));
  return { dir, lockDir: path.join(dir, HISTORY_LOCK_DIR) };
}

test('long-running live owner cannot be reaped because its directory mtime expired', async t => {
  const { dir, lockDir } = await fixture(t);
  const first = await acquireHistoryLock(dir, { timeoutMs: 250, retryMs: 5, staleMs: 10 });
  const old = new Date(Date.now() - 10_000);
  await utimes(lockDir, old, old);
  await assert.rejects(
    acquireHistoryLock(dir, { timeoutMs: 60, retryMs: 5, staleMs: 10 }),
    error => error instanceof HistoryError && error.code === 'HISTORY_LOCK_TIMEOUT'
  );
  assert.equal(JSON.parse(await readFile(path.join(lockDir, 'owner.json'), 'utf8')).pid, process.pid);
  await first.release();
  const second = await acquireHistoryLock(dir, { timeoutMs: 250, retryMs: 5, staleMs: 10 });
  await second.release();
});

test('orphan owner is reclaimed without stealing a different host owner', async t => {
  const { dir, lockDir } = await fixture(t);
  await mkdir(lockDir);
  const old = new Date(Date.now() - 10_000);
  await writeFile(path.join(lockDir, 'owner.json'), JSON.stringify({
    version: 1, token: 'orphan', pid: 99999999, hostname: hostname().toLowerCase()
  }));
  await utimes(lockDir, old, old);
  const recovered = await acquireHistoryLock(dir, { timeoutMs: 250, retryMs: 5, staleMs: 10 });
  await recovered.release();

  await mkdir(lockDir);
  await writeFile(path.join(lockDir, 'owner.json'), JSON.stringify({
    version: 1, token: 'remote', pid: 99999999, hostname: 'remote-host.example.invalid'
  }));
  await utimes(lockDir, old, old);
  await assert.rejects(
    acquireHistoryLock(dir, { timeoutMs: 60, retryMs: 5, staleMs: 10 }),
    error => error instanceof HistoryError && error.code === 'HISTORY_LOCK_TIMEOUT'
  );
});
