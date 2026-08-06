import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { loadConfigBundle } from '../dist/config.js';
import { ConfigStore } from '../dist/management.js';
import { readAgentSecrets } from '../dist/secrets.js';
import { createAgentRuntime } from '../dist/server.js';

function configDocument(root, dataDir) {
  return {
    host: '127.0.0.1',
    port: 3789,
    dataDir,
    permissionMode: 'trusted',
    toolProfile: 'core',
    management: { enabled: true },
    oauth: {
      clientId: 'chatgpt',
      password: 'restart-test-password',
      tokenSecret: 'restart-test-token-secret-that-is-long-enough'
    },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: {
      blockingConcurrency: 2,
      processConcurrency: 2,
      activeSessionLimit: 8,
      maxOutputBytes: 1024 * 1024
    }
  };
}

function managementToken(html) {
  return html.match(/name="ctmcp-admin-token" content="([A-Za-z0-9_-]+)"/)?.[1];
}

let portSequence = 0;
async function listen(server) {
  let lastError;
  for (let attempt = 0; attempt < 32; attempt += 1) {
    const port = 46_000 + ((process.pid + portSequence++) % 2_000);
    try {
      await new Promise((resolve, reject) => {
        const onError = error => {
          server.off('listening', onListening);
          reject(error);
        };
        const onListening = () => {
          server.off('error', onError);
          resolve();
        };
        server.once('error', onError);
        server.once('listening', onListening);
        server.listen(port, '127.0.0.1');
      });
      return port;
    } catch (error) {
      lastError = error;
      if (error?.code !== 'EADDRINUSE') throw error;
    }
  }
  throw lastError ?? new Error('unable to allocate restart test port');
}

async function startRuntime(t, requestRestart) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-restart-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-restart-data-'));
  const configPath = path.join(dataDir, 'agent.json');
  await writeFile(configPath, `${JSON.stringify(configDocument(root, dataDir), null, 2)}\n`);
  const loaded = await loadConfigBundle(configPath);
  loaded.config.port = 0;
  const configStore = new ConfigStore(loaded);
  const runtime = await createAgentRuntime(loaded.config, { configStore, requestRestart });
  const port = await listen(runtime.server);
  t.after(() => new Promise(resolve => runtime.server.close(resolve)));
  return { base: `http://127.0.0.1:${port}` };
}

async function tokenFor(base) {
  const html = await fetch(`${base}/ui`).then(response => response.text());
  const token = managementToken(html);
  assert.ok(token);
  return token;
}

test('management restart endpoint is token protected and dispatches after the response', async t => {
  let restartCalls = 0;
  const runtime = await startRuntime(t, () => { restartCalls += 1; });
  const token = await tokenFor(runtime.base);

  const statusResponse = await fetch(`${runtime.base}/admin/api/status`, {
    headers: { 'x-ctmcp-admin-token': token }
  });
  assert.equal(statusResponse.status, 200);
  const status = await statusResponse.json();
  assert.deepEqual(status.restart, { supported: true, mode: 'supervised' });

  const denied = await fetch(`${runtime.base}/admin/api/restart`, { method: 'POST' });
  assert.equal(denied.status, 403);
  assert.equal(restartCalls, 0);

  const response = await fetch(`${runtime.base}/admin/api/restart`, {
    method: 'POST',
    headers: {
      'x-ctmcp-admin-token': token,
      origin: runtime.base
    }
  });
  assert.equal(response.status, 202);
  assert.deepEqual(await response.json(), { ok: true, restarting: true });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(restartCalls, 1);
});

test('management restart endpoint reports when no supervisor is available', async t => {
  const runtime = await startRuntime(t, undefined);
  const token = await tokenFor(runtime.base);

  const status = await fetch(`${runtime.base}/admin/api/status`, {
    headers: { 'x-ctmcp-admin-token': token }
  }).then(response => response.json());
  assert.deepEqual(status.restart, { supported: false, mode: 'unavailable' });

  const response = await fetch(`${runtime.base}/admin/api/restart`, {
    method: 'POST',
    headers: {
      'x-ctmcp-admin-token': token,
      origin: runtime.base
    }
  });
  assert.equal(response.status, 409);
  const payload = await response.json();
  assert.equal(payload.error.code, 'RESTART_UNAVAILABLE');
  assert.match(payload.error.message, /start-node-agent\.bat/);
});

test('resolved built-in tunnel endpoint is persisted and clears the one-time enrollment secret', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-resolved-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-resolved-data-'));
  const configPath = path.join(dataDir, 'agent.json');
  const document = configDocument(root, dataDir);
  document.publicBaseUrl = 'https://tunnel.example/builtin/clients/provisional_1';
  document.tunnel = {
    enabled: true,
    publicUrl: 'https://tunnel.example/builtin/clients/provisional_1/mcp',
    enrollmentUrl: 'https://tunnel.example/_tunnel/enroll/ONETIME'
  };
  await writeFile(configPath, `${JSON.stringify(document, null, 2)}\n`);

  const loaded = await loadConfigBundle(configPath);
  const configStore = new ConfigStore(loaded);
  await configStore.applyResolvedBuiltinTunnel(
    'https://tunnel.example/builtin/clients/server_assigned_1/mcp',
    true
  );

  const saved = JSON.parse(await readFile(configPath, 'utf8'));
  assert.equal(saved.tunnel.publicUrl, 'https://tunnel.example/builtin/clients/server_assigned_1/mcp');
  assert.equal(saved.publicBaseUrl, 'https://tunnel.example/builtin/clients/server_assigned_1');
  assert.equal(loaded.config.tunnel.publicUrl, saved.tunnel.publicUrl);
  assert.equal(loaded.config.publicBaseUrl, saved.publicBaseUrl);
  assert.equal(loaded.config.tunnel.enrollmentUrl, undefined);
  const secretState = await readAgentSecrets(dataDir);
  assert.equal(secretState.secrets.tunnelEnrollmentUrl, undefined);
});

test('management rejects a malformed built-in tunnel public URL before saving', async t => {
  const runtime = await startRuntime(t, undefined);
  const token = await tokenFor(runtime.base);
  const snapshot = await fetch(`${runtime.base}/admin/api/config`, {
    headers: { 'x-ctmcp-admin-token': token }
  }).then(response => response.json());
  const saved = snapshot.saved;
  const response = await fetch(`${runtime.base}/admin/api/config`, {
    method: 'PUT',
    headers: {
      'x-ctmcp-admin-token': token,
      'content-type': 'application/json',
      origin: runtime.base
    },
    body: JSON.stringify({
      host: saved.host,
      port: saved.port,
      publicBaseUrl: saved.publicBaseUrl,
      dataDir: saved.dataDir,
      permissionMode: saved.permissionMode,
      toolProfile: saved.toolProfile,
      management: saved.management,
      oauth: {
        clientId: saved.oauth.clientId,
        password: '',
        clientSecret: '',
        clearClientSecret: false
      },
      folders: saved.folders,
      limits: saved.limits,
      tunnel: {
        enabled: true,
        publicUrl: 'https://tunnel.example/mcp',
        enrollmentUrl: '',
        clearEnrollmentUrl: false
      }
    })
  });
  assert.equal(response.status, 400);
  const payload = await response.json();
  assert.equal(payload.error.code, 'CONFIG_INVALID');
  assert.match(payload.error.message, /tunnel\.publicUrl is invalid/);
  assert.match(payload.error.message, /builtin\/clients/);
});
