import test from 'node:test';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { mkdir, readFile, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { ConversationHandleStore, CONVERSATION_HANDLE_TTL_MS, MAX_CONVERSATION_HANDLES, decorateHandleResult, decorateSessionStatus } from '../dist/server/mcp/conversationHandles.js';
import { createMcpFixture, mcpRequest } from './mcpTestHelpers.mjs';
import { redactSensitiveText, isSensitiveKey } from '../dist/redaction.js';

const bootstrap = (native, args = {}) => ({ method: 'tools/call', params: {
  name: 'conversation_bootstrap', arguments: args,
  ...(native ? { _meta: { 'openai/session': native } } : {})
} });

test('handle responses expose full data in one text block and preserve non-text content and errors', () => {
  const image = { type: 'image', data: 'aW1hZ2U=', mimeType: 'image/png' };
  const resource = { type: 'resource_link', uri: 'output://example', name: 'output' };
  const grant = { expiresAt: 1234, platformBound: false };
  for (const structured of [{ ok: true, files: ['one.txt', 'two.txt'] }, { ok: false, error: 'permission denied' }]) {
    const result = decorateHandleResult({ structuredContent: structured, isError: !structured.ok,
      content: [{ type: 'text', text: 'summary' }, image, { type: 'text', text: 'old metadata' }, resource] }, grant);
    const text = result.content.filter(block => block.type === 'text');
    assert.equal(text.length, 1);
    assert.deepEqual(JSON.parse(text[0].text), result.structuredContent);
    assert.deepEqual(result.structuredContent, { ...structured, conversation_handle_expires_at_ms: 1234,
      conversation_identity_source: 'conversation_handle', platform_conversation_bound: false });
    assert.deepEqual(result.content.slice(1), [image, resource]);
    assert.equal(result.isError, !structured.ok);
  }
  const legacy = decorateHandleResult({ isError: true, content: [{ type: 'text', text: 'original error' }] }, grant);
  assert.equal(JSON.parse(legacy.content[0].text).text, 'original error');
  assert.equal(legacy.isError, true);
  const task = { resultType: 'task', task: { taskId: 'retained-task' } };
  assert.deepEqual(decorateHandleResult(structuredClone(task), grant), task);
  const ordinary = { content: [{ type: 'text', text: 'unchanged' }] };
  assert.equal(decorateHandleResult(ordinary, undefined), ordinary);
});

test('session_status keeps expiry consistent between structured and text results', () => {
  const input = { structuredContent: { ok: true, oauth_token_expires_at_ms: null },
    content: [{ type: 'text', text: 'status' }, { type: 'resource_link', uri: 'output://sample' }] };
  const result = decorateSessionStatus(input, 54321);
  assert.equal(result.structuredContent.oauth_token_expires_at_ms, 54321);
  assert.deepEqual(JSON.parse(result.content[0].text), result.structuredContent);
  assert.equal(result.content[1].type, 'resource_link');
});

test('handle store enforces ownership, native identity, absolute TTL, restart and capacity', () => {
  const forgotten = [];
  const store = new ConversationHandleStore(key => forgotten.push(key));
  const body = bootstrap(undefined);
  const issued = store.prepare('owner', body, 100);
  assert.match(issued.token, /^ch_[a-f0-9]{64}$/);
  assert.equal(issued.expiresAt, 100 + CONVERSATION_HANDLE_TTL_MS);
  assert.deepEqual(store.expiryTimes(200), [issued.expiresAt]);
  const key = body.params._meta['openai/session'];
  const request = () => ({ method: 'tools/call', params: { name: 'list_workspace_folders', arguments: { conversation_handle: issued.token } } });
  const resumed = request();
  assert.equal(store.prepare('owner', resumed, 200).expiresAt, issued.expiresAt);
  assert.equal(resumed.params._meta['openai/session'], key);
  assert.equal('conversation_handle' in resumed.params.arguments, false);
  assert.throws(() => store.prepare('other', request(), 200), { rpcData: { error_code: 'CONVERSATION_HANDLE_INVALID', error_category: 'workspace_routing', retryable: false } });
  assert.throws(() => new ConversationHandleStore().prepare('owner', request(), 200));
  const conflict = request();
  conflict.params._meta = { 'openai/session': 'different-platform-chat' };
  assert.throws(() => store.prepare('owner', conflict, 200), /platform conversation/);
  assert.throws(() => store.prepare('owner', request(), issued.expiresAt), /expired/);
  assert.deepEqual(store.expiryTimes(issued.expiresAt), []);
  assert.deepEqual(forgotten, [key]);
  const capped = new ConversationHandleStore();
  for (let index = 0; index < MAX_CONVERSATION_HANDLES; index++) capped.prepare('owner', bootstrap(), 0);
  assert.throws(() => capped.prepare('owner', bootstrap(), 1), /capacity/);
  assert.ok(capped.prepare('owner', bootstrap(), CONVERSATION_HANDLE_TTL_MS).token);
  const native = bootstrap('chatgpt');
  assert.equal(store.prepare(undefined, native), undefined);
  assert.equal(native.params._meta['openai/session'], 'chatgpt');
  assert.throws(() => store.prepare(undefined, bootstrap()), /authentication/);
  assert.throws(() => store.prepare('owner', bootstrap('ctmcp-handle:spoof')), /Reserved/);
});

async function call(state, name, args = {}, options = {}) {
  const payload = { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name, arguments: args, ...(options.meta ? { _meta: options.meta } : {}) } };
  const response = await mcpRequest(state, payload, options);
  return response.json();
}

function alternateCredential(authorization) {
  const token = authorization.slice('Bearer '.length);
  const [header, body] = token.split('.');
  const payload = JSON.parse(Buffer.from(body, 'base64url'));
  payload.iat -= 1;
  const encoded = Buffer.from(JSON.stringify(payload)).toString('base64url');
  const signed = `${header}.${encoded}`;
  return `Bearer ${signed}.${createHmac('sha256', 'mcp-http-parity-token-secret').update(signed).digest('base64url')}`;
}

test('session_status shows authenticated caller token and handle expiry without exposing secrets', async t => {
  const state = await createMcpFixture(t);
  const first = await call(state, 'conversation_bootstrap', { folder_id: 'repo' });
  const handle = first.result.structuredContent.conversation_handle;
  const response = await call(state, 'session_status', { conversation_handle: handle });
  const result = response.result?.structuredContent;
  assert.equal(result?.ok, true, JSON.stringify(response));
  assert.equal(result.auth_type, 'oauth');
  assert.ok(result.oauth_token_expires_at_ms > Date.now());
  assert.equal(result.conversation_handle_expires_at_ms, first.result.structuredContent.conversation_handle_expires_at_ms);
  assert.deepEqual(JSON.parse(response.result.content[0].text), result);
  assert.equal(JSON.stringify(result).includes(handle), false);
});

test('authenticated HTTP handles preserve cwd, isolate new chats, strip secrets and reject fallback', async t => {
  const state = await createMcpFixture(t);
  await mkdir(path.join(state.root, 'nested'));
  await writeFile(path.join(state.root, 'nested', 'marker.txt'), 'handle nested file');
  state.runtime.context.config.securityPolicy.redactTelemetry = false;
  state.runtime.context.usageStore.setRedactTelemetry(false);
  const first = await call(state, 'conversation_bootstrap', { folder_id: 'repo' });
  assert.equal(first.result.structuredContent.ok, true, JSON.stringify(first));
  const handle = first.result.structuredContent.conversation_handle;
  assert.equal(first.result.content.filter(block => block.type === 'text').length, 1);
  assert.equal(JSON.parse(first.result.content[0].text).conversation_handle, handle);
  assert.match(handle, /^ch_[a-f0-9]{64}$/);
  assert.equal(first.result.structuredContent.platform_conversation_bound, false);
  assert.equal((await call(state, 'set_default_cwd', { conversation_handle: handle, path: 'nested' })).result.structuredContent.ok, true);
  const read = await call(state, 'read_file', { conversation_handle: handle, path: 'marker.txt' });
  assert.equal(read.result.structuredContent.content, 'handle nested file', JSON.stringify(read));
  assert.equal(read.result.content.filter(block => block.type === 'text').length, 1);
  assert.equal(JSON.parse(read.result.content[0].text).content, 'handle nested file');
  assert.deepEqual(JSON.parse(read.result.content[0].text), read.result.structuredContent);
  assert.equal(read.result.structuredContent.conversation_identity_source, 'conversation_handle');
  assert.equal('conversation_handle' in read.result.structuredContent, false);
  const second = (await call(state, 'conversation_bootstrap', { folder_id: 'repo' })).result.structuredContent;
  assert.notEqual(second.conversation_handle, handle);
  const secondListing = await call(state, 'list_workspace_folders', { conversation_handle: second.conversation_handle });
  assert.equal(secondListing.result.structuredContent.default_cwd, '.');
  assert.equal((await call(state, 'list_workspace_folders', {})).result.structuredContent.selected_folder_id, null);
  const differentOwner = await call(state, 'read_file', { conversation_handle: handle, workspace_folder_id: 'repo', path: 'marker.txt' }, { headers: { authorization: alternateCredential(state.authorization) } });
  assert.equal(differentOwner.error.data.error_code, 'CONVERSATION_HANDLE_INVALID');
  const conflict = await call(state, 'list_workspace_folders', { conversation_handle: handle }, { meta: { 'openai/session': 'other-chat' } });
  assert.equal(conflict.error.data.error_code, 'CONVERSATION_HANDLE_CONFLICT');
  const spoof = await call(state, 'list_workspace_folders', {}, { meta: { 'openai/session': 'ctmcp-handle:spoof' } });
  assert.equal(spoof.error.data.error_code, 'CONVERSATION_HANDLE_CONFLICT');
  const invalid = await call(state, 'read_file', { conversation_handle: 'invalid', workspace_folder_id: 'repo', path: 'marker.txt' });
  assert.equal(invalid.error.data.error_code, 'CONVERSATION_HANDLE_INVALID');
  const unauthorized = await mcpRequest(state, { jsonrpc: '2.0', id: 3, method: 'tools/call', params: { name: 'conversation_bootstrap', arguments: {} } }, { auth: false });
  assert.equal(unauthorized.status, 401);
  await state.runtime.context.conversations.flush();
  await state.runtime.context.usageStore.flush();
  const telemetry = await readFile(path.join(state.dataDir, 'logs', 'diagnostics.jsonl'), 'utf8');
  assert.equal(telemetry.includes(handle), false);
  const persistedPath = path.join(state.dataDir, `conversation-state-${state.runtime.context.workspaceProfileId}.json`);
  const persisted = await readFile(persistedPath, 'utf8');
  assert.equal(persisted.includes(handle), false);
  assert.equal(persisted.includes('ctmcp-handle:'), false);
  assert.equal(isSensitiveKey('conversation_handle'), true);
  assert.equal(redactSensitiveText(`reused ${handle}`).value.includes(handle), false);
});

test('native OpenAI calls opt in to handles without changing conversation routing', async t => {
  const state = await createMcpFixture(t);
  const meta = { 'openai/session': 'native-chat' };
  const normal = (await call(state, 'conversation_bootstrap', { folder_id: 'repo' }, { meta })).result.structuredContent;
  assert.equal('conversation_handle' in normal, false);
  const issued = (await call(state, 'conversation_bootstrap', { create_conversation_handle: true }, { meta })).result.structuredContent;
  assert.match(issued.conversation_handle, /^ch_[a-f0-9]{64}$/);
  assert.equal(issued.platform_conversation_bound, true);
  const read = await call(state, 'list_workspace_folders', { conversation_handle: issued.conversation_handle }, { meta });
  assert.equal(read.result.structuredContent.selected_folder_id, 'repo');
  const noMeta = await call(state, 'list_workspace_folders', { conversation_handle: issued.conversation_handle });
  assert.equal(noMeta.error.data.error_code, 'CONVERSATION_HANDLE_CONFLICT');
  const otherRuntime = await createMcpFixture(t);
  const crossRuntime = await call(otherRuntime, 'list_workspace_folders', { conversation_handle: issued.conversation_handle }, { meta });
  assert.equal(crossRuntime.error.data.error_code, 'CONVERSATION_HANDLE_INVALID');
});

test('handle-backed process controls remain usable without protocol task polling', async t => {
  const state = await createMcpFixture(t);
  const bootstrapResult = (await call(state, 'conversation_bootstrap', { folder_id: 'repo' })).result.structuredContent;
  const handle = bootstrapResult.conversation_handle;
  const meta = {
    'io.modelcontextprotocol/protocolVersion': '2026-07-28',
    'io.modelcontextprotocol/clientCapabilities': { extensions: { 'io.modelcontextprotocol/tasks': {} } }
  };
  const response = await call(state, 'exec_command', {
    conversation_handle: handle, program: path.basename(process.execPath),
    args: ['-e', 'setTimeout(()=>console.log("handle-process-done"),200)'],
    yield_time_ms: 0, timeout_ms: 5000, output_mode: 'full'
  }, { meta, headers: { 'mcp-protocol-version': '2026-07-28', 'mcp-method': 'tools/call', 'mcp-name': 'exec_command' } });
  assert.equal(response.result.resultType, 'complete', JSON.stringify(response));
  assert.equal('task' in response.result, false);
  const session = response.result.structuredContent.session_id;
  assert.ok(session, JSON.stringify(response));
  let finished;
  for (let attempt = 0; attempt < 20; attempt++) {
    finished = (await call(state, 'wait_command', { conversation_handle: handle, session_id: session, timeout_ms: 500, until: 'exit' })).result.structuredContent;
    if (finished.process_completed) break;
  }
  assert.equal(finished.process_completed, true, JSON.stringify(finished));
  assert.equal(finished.process_exit_code, 0, JSON.stringify(finished));
  assert.match(finished.stdout, /handle-process-done/);
});
