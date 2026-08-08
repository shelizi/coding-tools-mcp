import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash, createHmac } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  authorizationMetadata, externalBase, OAuthRuntime, redirectUriAllowed, resourceMetadata
} from '../dist/oauth.js';
import { createAgentRuntime } from '../dist/server.js';

const verifier = 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-._~';
const redirectUri = 'https://chatgpt.com/connector_platform_oauth_redirect';

function oauthConfig(overrides = {}) {
  return {
    clientId: 'chatgpt',
    password: 'oauth-test-password',
    tokenSecret: 'oauth-test-token-secret-that-is-long-enough',
    ...overrides
  };
}

function agentConfig(root, dataDir, publicBaseUrl = 'https://public.example/builtin/clients/oauth-test') {
  return {
    host: '127.0.0.1',
    port: 0,
    publicBaseUrl,
    dataDir,
    permissionMode: 'trusted',
    management: { enabled: false },
    oauth: oauthConfig(),
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 }
  };
}

function authorizationForm(state = 'state-1') {
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  return new URLSearchParams({
    client_id: 'chatgpt',
    redirect_uri: redirectUri,
    code_challenge: challenge,
    code_challenge_method: 'S256',
    state,
    password: 'oauth-test-password'
  });
}

function tokenForm(code) {
  return new URLSearchParams({
    grant_type: 'authorization_code',
    code,
    redirect_uri: redirectUri,
    code_verifier: verifier,
    client_id: 'chatgpt'
  });
}

function signToken(payload, secret) {
  const header = Buffer.from(JSON.stringify({ alg: 'HS256', typ: 'JWT' })).toString('base64url');
  const body = Buffer.from(JSON.stringify(payload)).toString('base64url');
  const signature = createHmac('sha256', secret).update(`${header}.${body}`).digest('base64url');
  return `${header}.${body}.${signature}`;
}

test('OAuth metadata, redirect allowlist and Forwarded base resolution match Rust', () => {
  const runtime = new OAuthRuntime(oauthConfig());
  const config = agentConfig('C:\\workspace', 'C:\\state', undefined);
  delete config.publicBaseUrl;
  config.port = 3789;
  assert.equal(externalBase({ forwarded: 'for=192.0.2.1;proto=https;host="mcp.example"' }, config), 'https://mcp.example');
  assert.equal(externalBase({ 'x-forwarded-proto': 'https', 'x-forwarded-host': 'proxy.example' }, config), 'https://proxy.example');

  assert.equal(redirectUriAllowed('https://chatgpt.com/connector/oauth/test'), true);
  assert.equal(redirectUriAllowed('https://chat.openai.com/aip/test/oauth/callback?source=connector'), true);
  assert.equal(redirectUriAllowed('https://chatgpt.com:443/connector/oauth/test'), true);
  for (const value of [
    'http://chatgpt.com/connector/oauth/test',
    'https://attacker.example/callback',
    'https://chatgpt.com.attacker.example/callback',
    'https://chatgpt.com@attacker.example/callback',
    'https://chatgpt.com:444/connector/oauth/test',
    'https://chatgpt.com/connector/oauth/test#fragment',
    ' https://chatgpt.com/connector/oauth/test'
  ]) assert.equal(redirectUriAllowed(value), false, value);

  assert.deepEqual(authorizationMetadata('https://mcp.example/base', runtime).grant_types_supported, ['authorization_code']);
  assert.deepEqual(authorizationMetadata('https://mcp.example/base', runtime).token_endpoint_auth_methods_supported, ['none']);
  assert.deepEqual(resourceMetadata('https://mcp.example/base').authorization_servers, ['https://mcp.example/base']);

  const page = runtime.authorizePage(new URL(`https://local/oauth/authorize?${new URLSearchParams({
    response_type: 'code', client_id: 'chatgpt', redirect_uri: redirectUri,
    code_challenge: 'challenge', code_challenge_method: 'S256', state: 'state'
  })}`));
  assert.equal(page.status, 200);
  assert.match(page.body, /method="POST" action=""/);
});

test('OAuthRuntime rejects missing credentials and ignores blank optional client secrets', () => {
  assert.throws(() => new OAuthRuntime(oauthConfig({ clientId: ' ' })), /OAuth client ID is not configured/);
  assert.throws(() => new OAuthRuntime(oauthConfig({ password: ' ' })), /OAuth password is not configured/);
  assert.throws(() => new OAuthRuntime(oauthConfig({ tokenSecret: ' ' })), /OAuth token secret is not configured/);
  assert.equal(new OAuthRuntime(oauthConfig({ clientSecret: ' ' })).clientSecret, undefined);
});

test('OAuthRuntime updates credentials and clears pending authorization codes', () => {
  const base = 'https://public.example/builtin/clients/oauth-test';
  const runtime = new OAuthRuntime(oauthConfig({ clientSecret: 'old-client-secret' }));
  const authorized = runtime.authorizeSubmit(authorizationForm('before-rotation'), base);
  const code = new URL(authorized.location).searchParams.get('code');
  assert.ok(code);

  runtime.update(oauthConfig({
    password: 'rotated-password',
    clientSecret: 'rotated-client-secret'
  }));
  assert.equal(runtime.password, 'rotated-password');
  assert.equal(runtime.clientSecret, 'rotated-client-secret');

  const form = tokenForm(code);
  form.set('client_secret', 'rotated-client-secret');
  assert.deepEqual(runtime.exchangeToken(form, {}, base).body, {
    error: 'invalid_grant',
    error_description: 'Unknown or already-used authorization code'
  });
});

test('issuing a new code removes expired pending codes like Rust', () => {
  let now = 0;
  const base = 'https://public.example/builtin/clients/oauth-test';
  const runtime = new OAuthRuntime(oauthConfig(), () => now);
  const oldCode = new URL(runtime.authorizeSubmit(authorizationForm('old'), base).location).searchParams.get('code');
  assert.ok(oldCode);
  now = 5 * 60_000 + 1;
  runtime.authorizeSubmit(authorizationForm('new'), base);
  assert.deepEqual(runtime.exchangeToken(tokenForm(oldCode), {}, base).body, {
    error: 'invalid_grant',
    error_description: 'Unknown or already-used authorization code'
  });
});

test('authorization codes are isolated per OAuthRuntime and single-use', () => {
  const base = 'https://public.example/builtin/clients/oauth-test';
  const first = new OAuthRuntime(oauthConfig());
  const second = new OAuthRuntime(oauthConfig());
  const authorized = first.authorizeSubmit(authorizationForm('state-isolated'), base);
  assert.equal(authorized.status, 303);
  const callback = new URL(authorized.location);
  assert.equal(callback.searchParams.get('state'), 'state-isolated');
  const code = callback.searchParams.get('code');
  assert.ok(code);

  const rejected = second.exchangeToken(tokenForm(code), {}, base);
  assert.deepEqual(rejected.body, {
    error: 'invalid_grant',
    error_description: 'Unknown or already-used authorization code'
  });

  const exchanged = first.exchangeToken(tokenForm(code), {}, base);
  assert.equal(exchanged.status, 200);
  const accessToken = exchanged.body.access_token;
  assert.equal(first.verifyBearer({ authorization: `Bearer ${accessToken}` }, base), true);
  assert.deepEqual(first.exchangeToken(tokenForm(code), {}, base).body, {
    error: 'invalid_grant',
    error_description: 'Unknown or already-used authorization code'
  });
});

test('optional client secret supports post and basic authentication without consuming codes on client errors', () => {
  const base = 'https://public.example/builtin/clients/oauth-test';
  const runtime = new OAuthRuntime(oauthConfig({ clientSecret: 'oauth-client-secret' }));
  assert.deepEqual(authorizationMetadata(base, runtime).token_endpoint_auth_methods_supported, [
    'client_secret_post', 'client_secret_basic'
  ]);

  const firstCode = new URL(runtime.authorizeSubmit(authorizationForm('secret-basic'), base).location).searchParams.get('code');
  assert.ok(firstCode);
  assert.deepEqual(runtime.exchangeToken(tokenForm(firstCode), {}, base).body, {
    error: 'invalid_client',
    error_description: 'Invalid client_secret'
  });
  const basicForm = tokenForm(firstCode);
  basicForm.delete('client_id');
  const basic = Buffer.from('chatgpt:oauth-client-secret').toString('base64');
  assert.equal(runtime.exchangeToken(basicForm, { authorization: `Basic ${basic}` }, base).status, 200);

  const secondCode = new URL(runtime.authorizeSubmit(authorizationForm('secret-post'), base).location).searchParams.get('code');
  assert.ok(secondCode);
  const postForm = tokenForm(secondCode);
  postForm.set('client_secret', 'oauth-client-secret');
  assert.equal(runtime.exchangeToken(postForm, {}, base).status, 200);
});

test('bearer verification follows Rust string audience and required claim types', () => {
  const base = 'https://public.example/builtin/clients/oauth-test';
  const runtime = new OAuthRuntime(oauthConfig());
  const now = Math.floor(Date.now() / 1000);
  const common = { iss: base, iat: now, exp: now + 300, scope: 'any-string' };
  const issuerAudience = signToken({ ...common, aud: base }, runtime.tokenSecret);
  assert.equal(runtime.verifyBearer({ authorization: `Bearer ${issuerAudience}` }, base), true);
  const arrayAudience = signToken({ ...common, aud: [`${base}/mcp`] }, runtime.tokenSecret);
  assert.equal(runtime.verifyBearer({ authorization: `Bearer ${arrayAudience}` }, base), false);
  const missingScope = signToken({ iss: base, aud: `${base}/mcp`, iat: now, exp: now + 300 }, runtime.tokenSecret);
  assert.equal(runtime.verifyBearer({ authorization: `Bearer ${missingScope}` }, base), false);
});

test('closing an Agent runtime clears its pending authorization codes', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-oauth-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-oauth-state-'));
  t.after(async () => {
    await rm(root, { recursive: true, force: true });
    await rm(dataDir, { recursive: true, force: true });
  });
  const config = agentConfig(root, dataDir);
  const runtime = await createAgentRuntime(config);
  await new Promise(resolve => runtime.server.listen(0, '127.0.0.1', resolve));
  const address = runtime.server.address();
  assert.ok(address && typeof address === 'object');
  const response = await fetch(`http://127.0.0.1:${address.port}/builtin/clients/oauth-test/oauth/authorize`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: authorizationForm('state-close'),
    redirect: 'manual'
  });
  assert.equal(response.status, 303);
  const code = new URL(response.headers.get('location')).searchParams.get('code');
  assert.ok(code);
  await new Promise(resolve => runtime.server.close(resolve));

  assert.deepEqual(runtime.oauth.exchangeToken(tokenForm(code), {}, config.publicBaseUrl).body, {
    error: 'invalid_grant',
    error_description: 'Unknown or already-used authorization code'
  });
});
