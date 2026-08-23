import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createMcpFixture } from './mcpTestHelpers.mjs';

test('server composition delegates route families and MCP method dispatch', async () => {
  const source = await readFile(new URL('../src/server.ts', import.meta.url), 'utf8');

  assert.match(source, /handleOAuthRoute/);
  assert.match(source, /handleSystemRoute/);
  assert.match(source, /handleMcpRoute/);
  assert.doesNotMatch(source, /method === 'tools\/call'/);
  assert.doesNotMatch(source, /validateJsonRpcMessage/);
  assert.doesNotMatch(source, /new ProcessRequestLifecycle/);
});

test('server route boundaries preserve scoped system, OAuth, and fallback responses', async t => {
  const state = await createMcpFixture(t, {
    publicBaseUrl: 'https://public.example/builtin/clients/route-test'
  });
  const prefix = '/builtin/clients/route-test';

  const health = await fetch(`${state.localBase}${prefix}/health`);
  assert.equal(health.status, 200);
  assert.equal(health.headers.get('cache-control'), 'no-store');
  assert.equal(health.headers.get('x-content-type-options'), 'nosniff');
  assert.equal((await health.json()).server, 'coding-tools-mcp-node');

  const metadata = await fetch(
    `${state.localBase}/.well-known/oauth-authorization-server${prefix}`
  );
  assert.equal(metadata.status, 200);
  assert.equal((await metadata.json()).issuer, `https://public.example${prefix}`);

  const authorize = await fetch(`${state.localBase}${prefix}/oauth/authorize`, {
    redirect: 'manual'
  });
  assert.equal(authorize.headers.get('content-type'), 'text/html; charset=utf-8');
  assert.equal(authorize.headers.get('cache-control'), 'no-store');
  assert.equal(authorize.headers.get('x-content-type-options'), 'nosniff');

  const missing = await fetch(`${state.localBase}${prefix}/missing`);
  assert.equal(missing.status, 404);
  assert.equal(missing.headers.get('content-type'), 'text/plain; charset=utf-8');
  assert.equal(missing.headers.get('cache-control'), 'no-store');
  assert.equal(missing.headers.get('x-content-type-options'), 'nosniff');
  assert.equal(await missing.text(), 'Not found');
});
