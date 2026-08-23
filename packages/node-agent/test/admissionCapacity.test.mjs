import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createToolContext } from '../dist/server.js';
import { disposeProcessSessions, ProcessRequestLifecycle } from '../dist/processes.js';
import { callTool } from '../dist/tools.js';

function config(folders, dataDir) {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode: 'trusted',
    management: { enabled: false },
    oauth: { clientId: 'chatgpt', password: 'admission-test-password', tokenSecret: 'admission-test-token-secret' },
    folders,
    limits: {
      blockingConcurrency: 1,
      processConcurrency: 1,
      globalBlockingConcurrency: 1,
      globalProcessConcurrency: 1,
      activeSessionLimit: 512,
      maxOutputBytes: 1024 * 1024
    }
  };
}

async function waitFor(read, timeoutMs = 2_000) {
  const started = Date.now();
  while (Date.now() - started < timeoutMs) {
    const value = read();
    if (value) return value;
    await new Promise(resolve => setTimeout(resolve, 5));
  }
  throw new Error('timed out waiting for admission state');
}

async function fixture(t) {
  const rootA = await mkdtemp(path.join(tmpdir(), 'ctmcp-admission-a-'));
  const rootB = await mkdtemp(path.join(tmpdir(), 'ctmcp-admission-b-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-admission-data-'));
  await writeFile(path.join(rootA, 'value.txt'), 'a');
  await writeFile(path.join(rootB, 'value.txt'), 'b');
  const ctx = await createToolContext(config([
    { id: 'a', name: 'A', path: rootA },
    { id: 'b', name: 'B', path: rootB }
  ], dataDir));
  t.after(async () => {
    await disposeProcessSessions(ctx);
    await Promise.allSettled([ctx.conversations.flush(), ctx.usageStore.flush()]);
    const remove = (target) => rm(target, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await remove(rootA);
    await remove(rootB);
    await remove(dataDir);
  });
  return { ctx };
}

test('global admission is acquired before workspace capacity', async t => {
  const { ctx } = await fixture(t);
  const metaA = { 'openai/session': 'admission-a' };
  const metaB = { 'openai/session': 'admission-b' };
  await callTool(ctx, 'switch_workspace_folder', { folder_id: 'a' }, metaA);
  await callTool(ctx, 'switch_workspace_folder', { folder_id: 'b' }, metaB);

  const releaseGlobal = await ctx.hubAdmission.blocking.acquire();
  const requestA = callTool(ctx, 'read_file', { path: 'value.txt' }, metaA);
  const requestB = callTool(ctx, 'read_file', { path: 'value.txt' }, metaB);
  await waitFor(() => ctx.hubAdmission.blocking.queued === 2);

  assert.equal(ctx.folderRuntimes.get('a').admission.blocking.active, 0);
  assert.equal(ctx.folderRuntimes.get('b').admission.blocking.active, 0);
  assert.equal(ctx.folderRuntimes.get('a').admission.blocking.queued, 0);
  assert.equal(ctx.folderRuntimes.get('b').admission.blocking.queued, 0);

  releaseGlobal();
  const [resultA, resultB] = await Promise.all([requestA, requestB]);
  for (const result of [resultA, resultB]) {
    assert.equal(result.ok, true, JSON.stringify(result));
    assert.equal(result.admission_scope, 'global_and_workspace');
    assert.equal(result.admission_lane, 'blocking');
    assert.equal(typeof result.global_admission_wait_ms, 'number');
    assert.equal(typeof result.workspace_admission_wait_ms, 'number');
    assert.equal(result.admission_queue_wait_ms, result.global_admission_wait_ms + result.workspace_admission_wait_ms);
  }
  assert.equal(ctx.hubAdmission.blocking.active, 0);
  assert.equal(ctx.folderRuntimes.get('a').admission.blocking.active, 0);
  assert.equal(ctx.folderRuntimes.get('b').admission.blocking.active, 0);
});

test('cancelling an admission wait removes the waiter without releasing another permit', async t => {
  const { ctx } = await fixture(t);
  const meta = { 'openai/session': 'admission-cancel' };
  await callTool(ctx, 'switch_workspace_folder', { folder_id: 'a' }, meta);

  const releaseGlobal = await ctx.hubAdmission.blocking.acquire();
  const lifecycle = new ProcessRequestLifecycle(ctx);
  const pending = callTool(ctx, 'read_file', { path: 'value.txt' }, meta, false, lifecycle);
  await waitFor(() => ctx.hubAdmission.blocking.queued === 1);
  lifecycle.abort();
  const result = await pending;

  assert.equal(result.ok, false);
  assert.equal(ctx.hubAdmission.blocking.queued, 0);
  assert.equal(ctx.hubAdmission.blocking.active, 1);
  assert.equal(ctx.folderRuntimes.get('a').admission.blocking.active, 0);
  releaseGlobal();
  releaseGlobal();
  assert.equal(ctx.hubAdmission.blocking.active, 0);
});
