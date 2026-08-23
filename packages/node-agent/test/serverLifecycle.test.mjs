import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createAgentRuntime } from '../dist/server.js';

function deferred() {
  let resolve;
  const promise = new Promise(done => {
    resolve = done;
  });
  return { promise, resolve };
}

function config(root, dataDir) {
  return {
    host: '127.0.0.1',
    port: 0,
    dataDir,
    permissionMode: 'trusted',
    management: { enabled: false },
    oauth: {
      clientId: 'chatgpt',
      password: 'server-lifecycle-password',
      tokenSecret: 'server-lifecycle-token-secret-that-is-long-enough'
    },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: {
      blockingConcurrency: 4,
      processConcurrency: 4,
      activeSessionLimit: 16,
      maxOutputBytes: 1024 * 1024
    }
  };
}

test('AgentRuntime close is idempotent and waits for durable store flushes', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-runtime-close-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-runtime-close-data-'));
  const runtimeRegistry = new Map();
  const runtime = await createAgentRuntime(config(root, dataDir), { runtimeRegistry });
  t.after(async () => {
    await runtime.close();
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });

  const conversationGate = deferred();
  const usageGate = deferred();
  const conversationStarted = deferred();
  const usageStarted = deferred();
  const originalConversationFlush = runtime.context.conversations.flush.bind(runtime.context.conversations);
  const originalUsageFlush = runtime.context.usageStore.flush.bind(runtime.context.usageStore);
  let conversationFlushes = 0;
  let usageFlushes = 0;
  runtime.context.conversations.flush = async () => {
    conversationFlushes += 1;
    conversationStarted.resolve();
    await conversationGate.promise;
    await originalConversationFlush();
  };
  runtime.context.usageStore.flush = async () => {
    usageFlushes += 1;
    usageStarted.resolve();
    await usageGate.promise;
    await originalUsageFlush();
  };

  await new Promise((resolve, reject) => {
    runtime.server.once('error', reject);
    runtime.server.listen(0, '127.0.0.1', resolve);
  });
  assert.equal(runtimeRegistry.size, 1);

  const firstClose = runtime.close();
  const secondClose = runtime.close();
  assert.equal(firstClose, secondClose);
  let settled = false;
  void firstClose.finally(() => {
    settled = true;
  });
  await Promise.all([conversationStarted.promise, usageStarted.promise]);
  assert.equal(settled, false);
  assert.equal(runtimeRegistry.size, 0);

  conversationGate.resolve();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(settled, false);
  usageGate.resolve();
  await firstClose;
  await runtime.close();

  assert.equal(conversationFlushes, 1);
  assert.equal(usageFlushes, 1);
  assert.equal(runtime.server.listening, false);
});
