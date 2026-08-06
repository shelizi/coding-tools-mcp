import { createHash, randomBytes } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createAgentRuntime } from '../dist/server.js';
import { BuiltinTunnelManager, parseBuiltinPublicUrl } from '../dist/tunnel.js';

function required(name) {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required for the production Built-in WSS E2E`);
  return value;
}

function boundedInteger(name, fallback, minimum, maximum) {
  const raw = process.env[name];
  if (!raw) return fallback;
  const value = Number(raw);
  if (!Number.isInteger(value) || value < minimum || value > maximum) {
    throw new Error(`${name} must be an integer between ${minimum} and ${maximum}`);
  }
  return value;
}

async function waitFor(predicate, timeoutMs, label) {
  const started = Date.now();
  let lastError;
  while (Date.now() - started < timeoutMs) {
    try {
      const value = await predicate();
      if (value) return value;
    } catch (error) { lastError = error; }
    await new Promise(resolve => setTimeout(resolve, 500));
  }
  throw new Error(`${label} did not become ready within ${timeoutMs}ms${lastError ? `: ${lastError.message}` : ''}`);
}

async function fetchChecked(url, init, expected = [200]) {
  const response = await fetch(url, { ...init, redirect: init?.redirect ?? 'manual' });
  if (!expected.includes(response.status)) {
    const body = (await response.text()).slice(0, 4096);
    throw new Error(`${init?.method ?? 'GET'} ${new URL(url).pathname} returned ${response.status}: ${body}`);
  }
  return response;
}

async function rpc(endpoint, token, request) {
  const response = await fetchChecked(endpoint, {
    method: 'POST',
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: JSON.stringify(request)
  });
  const payload = await response.json();
  if (payload.error) throw new Error(`MCP ${request.method} failed: ${JSON.stringify(payload.error)}`);
  return payload.result;
}

const publicUrl = required('CTMCP_E2E_BUILTIN_PUBLIC_URL');
const oauthPassword = required('CTMCP_E2E_OAUTH_PASSWORD');
const endpoint = parseBuiltinPublicUrl(publicUrl);
const enrollmentUrl = process.env.CTMCP_E2E_BUILTIN_ENROLLMENT_URL?.trim();
const clientId = process.env.CTMCP_E2E_OAUTH_CLIENT_ID?.trim() || 'chatgpt';
const clientSecret = process.env.CTMCP_E2E_OAUTH_CLIENT_SECRET?.trim();
const redirectUri = process.env.CTMCP_E2E_REDIRECT_URI?.trim() || 'https://chatgpt.com/connector_platform_oauth_redirect';
const timeoutMs = boundedInteger('CTMCP_E2E_TIMEOUT_MS', 90_000, 10_000, 600_000);
const suppliedDataDir = process.env.CTMCP_E2E_DATA_DIR?.trim();
const tokenSecret = process.env.CTMCP_E2E_OAUTH_TOKEN_SECRET?.trim()
  || (suppliedDataDir ? '' : randomBytes(48).toString('base64url'));
if (!tokenSecret.trim()) {
  throw new Error('CTMCP_E2E_OAUTH_TOKEN_SECRET is required when CTMCP_E2E_DATA_DIR is persistent');
}
if (!suppliedDataDir && !enrollmentUrl) {
  throw new Error('CTMCP_E2E_BUILTIN_ENROLLMENT_URL is required for an ephemeral production E2E identity');
}

const dataDir = suppliedDataDir || await mkdtemp(path.join(tmpdir(), 'ctmcp-production-wss-state-'));
const workspace = await mkdtemp(path.join(tmpdir(), 'ctmcp-production-wss-workspace-'));
const marker = `production-wss-${Date.now()}-${randomBytes(8).toString('hex')}`;
await writeFile(path.join(workspace, 'production-e2e.txt'), marker);
const identityFile = path.join(dataDir, 'production-wss-identity.enc.json');
let hasIdentity = false;
try { await readFile(identityFile); hasIdentity = true; }
catch (error) {
  if (error?.code !== 'ENOENT') {
    await rm(workspace, { recursive: true, force: true });
    if (!suppliedDataDir) await rm(dataDir, { recursive: true, force: true });
    throw error;
  }
}
if (!hasIdentity && !enrollmentUrl) {
  await rm(workspace, { recursive: true, force: true });
  if (!suppliedDataDir) await rm(dataDir, { recursive: true, force: true });
  throw new Error('CTMCP_E2E_BUILTIN_ENROLLMENT_URL is required when the persistent production E2E identity does not exist');
}

const config = {
  host: '127.0.0.1',
  port: 0,
  publicBaseUrl: endpoint.baseUrl,
  dataDir,
  permissionMode: 'trusted',
  management: { enabled: false },
  oauth: { clientId, password: oauthPassword, ...(clientSecret ? { clientSecret } : {}), tokenSecret },
  folders: [{ id: 'production-e2e', name: 'Production E2E', path: workspace }],
  limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 },
  tunnel: {
    enabled: true,
    publicUrl,
    ...(enrollmentUrl ? { enrollmentUrl } : {}),
    stateFile: identityFile
  }
};

let server;
let context;
let tunnel;
let serverListening = false;
try {
  ({ server, context } = await createAgentRuntime(config));
  tunnel = new BuiltinTunnelManager(config, context);
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, config.host, () => { server.off('error', reject); resolve(); });
  });
  serverListening = true;
  const address = server.address();
  if (!address || typeof address === 'string') throw new Error('Agent did not expose a TCP address');
  config.port = address.port;
  await tunnel.start();

  await waitFor(
    () => context.tunnelStatus?.connectedWorkers > 0,
    timeoutMs,
    `production WSS worker (${context.tunnelStatus?.lastError ?? 'no server error'})`
  );

  const origin = new URL(endpoint.publicUrl).origin;
  const prefix = new URL(endpoint.baseUrl).pathname.replace(/\/$/, '');
  const authorizationMetadataUrl = `${origin}/.well-known/oauth-authorization-server${prefix}`;
  const resourceMetadataUrl = `${origin}/.well-known/oauth-protected-resource${prefix}/mcp`;
  const metadata = await waitFor(async () => {
    const response = await fetch(authorizationMetadataUrl, { redirect: 'manual' });
    if (response.status !== 200) return undefined;
    return response.json();
  }, timeoutMs, 'public OAuth metadata');
  if (metadata.issuer !== endpoint.baseUrl) throw new Error(`Unexpected OAuth issuer: ${metadata.issuer}`);
  const resource = await (await fetchChecked(resourceMetadataUrl)).json();
  if (resource.resource !== endpoint.publicUrl) throw new Error(`Unexpected protected resource: ${resource.resource}`);

  const verifier = randomBytes(48).toString('base64url');
  const challenge = createHash('sha256').update(verifier).digest('base64url');
  const state = randomBytes(16).toString('base64url');
  const authorizeForm = new URLSearchParams({
    client_id: clientId,
    redirect_uri: redirectUri,
    code_challenge: challenge,
    code_challenge_method: 'S256',
    state,
    password: oauthPassword
  });
  const authorized = await fetchChecked(`${endpoint.baseUrl}/oauth/authorize`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: authorizeForm,
    redirect: 'manual'
  }, [303]);
  const location = authorized.headers.get('location');
  if (!location) throw new Error('OAuth authorize response did not include a redirect');
  const callback = new URL(location);
  if (callback.searchParams.get('state') !== state) throw new Error('OAuth state did not round-trip');
  const code = callback.searchParams.get('code');
  if (!code) throw new Error(`OAuth authorization failed: ${callback.searchParams.get('error') ?? 'missing code'}`);

  const tokenForm = new URLSearchParams({
    grant_type: 'authorization_code',
    code,
    redirect_uri: redirectUri,
    code_verifier: verifier,
    client_id: clientId,
    ...(clientSecret ? { client_secret: clientSecret } : {})
  });
  const tokenPayload = await (await fetchChecked(`${endpoint.baseUrl}/oauth/token`, {
    method: 'POST',
    headers: { 'content-type': 'application/x-www-form-urlencoded' },
    body: tokenForm
  })).json();
  if (!tokenPayload.access_token) throw new Error('OAuth token endpoint did not return an access token');

  const initialized = await rpc(endpoint.publicUrl, tokenPayload.access_token, {
    jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: '2025-11-25' }
  });
  if (initialized.protocolVersion !== '2025-11-25') throw new Error(`Unexpected MCP protocol: ${initialized.protocolVersion}`);
  const listed = await rpc(endpoint.publicUrl, tokenPayload.access_token, {
    jsonrpc: '2.0', id: 2, method: 'tools/list', params: {}
  });
  if (listed.tools?.length !== 50) throw new Error(`Expected 50 MCP tools, received ${listed.tools?.length}`);

  const meta = { 'openai/session': `production-e2e-${Date.now()}` };
  const folders = await rpc(endpoint.publicUrl, tokenPayload.access_token, {
    jsonrpc: '2.0', id: 3, method: 'tools/call',
    params: { name: 'list_workspace_folders', arguments: {}, _meta: meta }
  });
  if (folders.structuredContent?.folders?.length !== 1) throw new Error('Public MCP did not return the E2E workspace');
  await rpc(endpoint.publicUrl, tokenPayload.access_token, {
    jsonrpc: '2.0', id: 4, method: 'tools/call',
    params: { name: 'switch_workspace_folder', arguments: { folder_id: 'production-e2e' }, _meta: meta }
  });
  const read = await rpc(endpoint.publicUrl, tokenPayload.access_token, {
    jsonrpc: '2.0', id: 5, method: 'tools/call',
    params: { name: 'read_file', arguments: { path: 'production-e2e.txt' }, _meta: meta }
  });
  if (read.structuredContent?.content !== marker) throw new Error('Public MCP read_file did not round-trip the E2E marker');

  console.log(JSON.stringify({
    ok: true,
    public_origin: origin,
    client_id: endpoint.clientId,
    protocol_version: initialized.protocolVersion,
    tools: listed.tools.length,
    connected_workers: context.tunnelStatus?.connectedWorkers ?? 0,
    policy_revision: context.tunnelStatus?.policyRevision ?? null,
    completed_tunnel_requests: context.tunnelStatus?.completedRequests ?? 0,
    identity_reused: hasIdentity
  }, null, 2));
} finally {
  await tunnel?.stop().catch(() => undefined);
  if (serverListening && server) await new Promise(resolve => server.close(() => resolve()));
  await rm(workspace, { recursive: true, force: true });
  if (!suppliedDataDir) await rm(dataDir, { recursive: true, force: true });
}
