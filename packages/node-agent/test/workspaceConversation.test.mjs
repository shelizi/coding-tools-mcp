import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, realpath, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  ConversationStore, markMcpConversationMetadata, MAX_CONVERSATION_CONTEXTS
} from '../dist/conversation.js';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';
import { sameWorkspacePath, workspacePathIdentity } from '../dist/wsl.js';

function config(folders, dataDir) {
  return {
    host: '127.0.0.1',
    port: 0,
    dataDir,
    permissionMode: 'trusted',
    oauth: {
      clientId: 'chatgpt',
      password: 'workspace-conversation-test',
      tokenSecret: 'workspace-conversation-test-token'
    },
    folders,
    limits: {
      blockingConcurrency: 4,
      processConcurrency: 4,
      globalBlockingConcurrency: 8,
      globalProcessConcurrency: 8,
      activeSessionLimit: 16,
      maxOutputBytes: 1024 * 1024
    }
  };
}

async function fixture(t) {
  const base = await mkdtemp(path.join(tmpdir(), 'ctmcp-conversation-'));
  const first = path.join(base, 'first');
  const second = path.join(base, 'second');
  const dataDir = path.join(base, 'data');
  await Promise.all([
    mkdir(path.join(first, 'src'), { recursive: true }),
    mkdir(path.join(second, 'packages'), { recursive: true }),
    mkdir(dataDir, { recursive: true })
  ]);
  const folders = [
    { id: 'first', name: 'First', path: first },
    { id: 'second', name: 'Second', path: second }
  ];
  const ctx = await createToolContext(config(folders, dataDir));
  t.after(async () => {
    await ctx.conversations.flush();
    await ctx.usageStore.flush();
    await rm(base, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return { base, first: await realpath(first), second: await realpath(second), dataDir, folders, ctx };
}

test('workspace listing and switching expose conversation-isolated Rust metadata', async t => {
  const state = await fixture(t);
  const unscoped = await callTool(state.ctx, 'list_workspace_folders', {}, undefined);
  assert.equal(unscoped.ok, true);
  assert.equal(unscoped.multi_folder, true);
  assert.equal(unscoped.selected_folder_id, null);
  assert.equal(unscoped.selection_scope, 'unselected');
  assert.equal(unscoped.conversation_isolated, false);
  assert.equal(unscoped.default_cwd, null);
  assert.match(unscoped.profile_id, /^node-[0-9a-f]{16}$/);
  assert.deepEqual(unscoped.folders.map(folder => folder.selected), [false, false]);
  assert.ok(unscoped.folders.every(folder => folder.history_dir.endsWith(path.join('docs', 'history-session'))));

  const legacy = await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, undefined);
  assert.equal(legacy.ok, true);
  assert.equal(legacy.selection_scope, 'runtime');
  assert.equal(legacy.conversation_isolated, false);
  assert.equal(legacy.conversation_source, 'stable_fallback');

  const missingMcpMeta = markMcpConversationMetadata(undefined);
  const missingListing = await callTool(state.ctx, 'list_workspace_folders', {}, missingMcpMeta);
  assert.equal(missingListing.selected_folder_id, null);
  assert.equal(missingListing.selection_scope, 'unselected');
  assert.equal(missingListing.conversation_source, 'missing_mcp_conversation');
  const denied = await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, missingMcpMeta);
  assert.equal(denied.ok, false);
  assert.equal(denied.error.code, 'WORKSPACE_FOLDER_NOT_SELECTED');
  assert.equal(denied.error.category, 'workspace_routing');
  assert.equal(state.ctx.selections.size, 1);

  const isolatedRuntime = await createToolContext(config(state.folders, path.join(state.base, 'isolated-runtime')));
  assert.notEqual(isolatedRuntime.conversations.fallbackKey, state.ctx.conversations.fallbackKey);
  const isolatedListing = await callTool(isolatedRuntime, 'list_workspace_folders', {}, undefined);
  assert.equal(isolatedListing.selected_folder_id, null);
  await isolatedRuntime.conversations.flush();
  await isolatedRuntime.usageStore.flush();

  const metaA = { 'openai/session': 'conversation-a' };
  const metaB = { 'openai/session': 'conversation-b' };
  const selectedA = await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, metaA);
  const selectedB = await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'second' }, metaB);
  assert.equal(selectedA.selection_scope, 'conversation');
  assert.equal(selectedA.conversation_isolated, true);
  assert.equal(selectedA.default_cwd, '.');
  assert.equal(selectedA.profile_id, unscoped.profile_id);
  assert.equal(selectedA.history_dir, path.join(state.first, 'docs', 'history-session'));
  assert.equal(selectedB.selected_folder_id, 'second');

  const listedA = await callTool(state.ctx, 'list_workspace_folders', {}, metaA);
  const listedB = await callTool(state.ctx, 'list_workspace_folders', {}, metaB);
  assert.equal(listedA.selected_folder_id, 'first');
  assert.equal(listedB.selected_folder_id, 'second');
  assert.deepEqual(listedA.folders.map(folder => folder.selected), [true, false]);
  assert.deepEqual(listedB.folders.map(folder => folder.selected), [false, true]);
});

test('switching back to a folder restores that conversation folder cwd', async t => {
  const state = await fixture(t);
  const meta = { 'openai/session': 'cwd-conversation' };

  await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, meta);
  const firstCwd = await callTool(state.ctx, 'set_default_cwd', { path: 'src' }, meta);
  assert.equal(firstCwd.default_cwd, 'src');

  await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'second' }, meta);
  const secondCwd = await callTool(state.ctx, 'set_default_cwd', { path: 'packages' }, meta);
  assert.equal(secondCwd.default_cwd, 'packages');

  const restoredFirst = await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, meta);
  assert.equal(restoredFirst.default_cwd, 'src');
  assert.equal(restoredFirst.resolved_cwd, path.join(state.first, 'src'));

  const listing = await callTool(state.ctx, 'list_workspace_folders', {}, meta);
  assert.equal(listing.default_cwd, 'src');
  assert.equal(listing.folders.find(folder => folder.id === 'first').default_cwd, 'src');
  assert.equal(listing.folders.find(folder => folder.id === 'second').default_cwd, 'packages');
});

test('workspace selection, per-folder cwd, and fallback identity survive Agent restart', async t => {
  const state = await fixture(t);
  const meta = { 'openai/session': 'restart-conversation' };

  await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, meta);
  await callTool(state.ctx, 'set_default_cwd', { path: 'src' }, meta);
  await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'second' }, meta);
  await callTool(state.ctx, 'set_default_cwd', { path: 'packages' }, meta);
  await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'first' }, meta);
  await callTool(state.ctx, 'switch_workspace_folder', { folder_id: 'second' }, undefined);
  await state.ctx.conversations.flush();

  const restarted = await createToolContext(config(state.folders, state.dataDir));
  t.after(async () => {
    await restarted.conversations.flush();
    await restarted.usageStore.flush();
  });
  assert.equal(restarted.conversations.fallbackKey, state.ctx.conversations.fallbackKey);

  const restored = await callTool(restarted, 'list_workspace_folders', {}, meta);
  assert.equal(restored.selected_folder_id, 'first');
  assert.equal(restored.default_cwd, 'src');
  assert.equal(restored.folders.find(folder => folder.id === 'second').default_cwd, 'packages');
  const restoredSecond = await callTool(restarted, 'switch_workspace_folder', { folder_id: 'second' }, meta);
  assert.equal(restoredSecond.default_cwd, 'packages');

  const fallback = await callTool(restarted, 'list_workspace_folders', {}, undefined);
  assert.equal(fallback.selected_folder_id, 'second');
});

test('conversation contexts use deterministic 128-entry LRU while preserving bindings and cwd', () => {
  const store = new ConversationStore(MAX_CONVERSATION_CONTEXTS);
  store.selectFolder('conversation-000', 'repo');
  store.setCurrentCwd('conversation-000', 'remembered');
  for (let index = 1; index <= MAX_CONVERSATION_CONTEXTS + 160; index += 1) {
    store.selectFolder(`conversation-${String(index).padStart(3, '0')}`, 'repo');
  }

  assert.equal(store.activeContextCount, MAX_CONVERSATION_CONTEXTS);
  assert.equal(store.selectionCount, MAX_CONVERSATION_CONTEXTS + 161);
  assert.equal(store.savedCwdCount, 161);
  assert.equal(store.isContextActive('conversation-000', 'repo'), false);
  assert.equal(store.isContextActive('conversation-288', 'repo'), true);
  assert.equal(store.selectedFolder('conversation-000'), 'repo');

  assert.equal(store.currentCwd('conversation-000'), 'remembered');
  assert.equal(store.activeContextCount, MAX_CONVERSATION_CONTEXTS);
  assert.equal(store.isContextActive('conversation-000', 'repo'), true);
  assert.equal(store.isContextActive('conversation-161', 'repo'), false);
  assert.equal(store.savedCwdCount, 161);
});

test('equivalent Windows, UNC, and WSL roots share a stable identity', () => {
  assert.equal(
    sameWorkspacePath(String.raw`C:\Work\Repo`, String.raw`\\?\C:\work\repo`),
    true
  );
  assert.equal(
    sameWorkspacePath(String.raw`\\SERVER\Share\Repo`, String.raw`\\?\UNC\server\share\repo`),
    true
  );
  assert.equal(
    sameWorkspacePath(
      String.raw`\\wsl$\Ubuntu-24.04\opt\src\Project`,
      String.raw`\\wsl.localhost\ubuntu-24.04\opt\src\Project`
    ),
    true
  );
  assert.equal(
    sameWorkspacePath(
      String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\Project`,
      String.raw`\\wsl.localhost\Ubuntu-24.04\opt\src\project`
    ),
    false
  );
  assert.equal(
    workspacePathIdentity(String.raw`\\wsl$\Ubuntu-24.04\opt\src\Project`),
    workspacePathIdentity(String.raw`\\wsl.localhost\ubuntu-24.04\opt\src\Project`)
  );
});

test('canonical duplicate roots are rejected and profile identity survives restart', async t => {
  const state = await fixture(t);
  const duplicateConfig = config([
    { id: 'primary', name: 'Primary', path: state.first },
    { id: 'alias', name: 'Alias', path: path.join(state.first, '.') }
  ], path.join(state.base, 'duplicate-data'));
  await assert.rejects(
    createToolContext(duplicateConfig),
    error => error?.code === 'WORKSPACE_FOLDER_DUPLICATE_ROOT'
  );

  const physicalAlias = path.join(state.base, 'physical-alias');
  await symlink(state.first, physicalAlias, process.platform === 'win32' ? 'junction' : 'dir');
  const physicalDuplicateConfig = config([
    { id: 'physical', name: 'Physical', path: state.first },
    { id: 'symlink', name: 'Symlink', path: physicalAlias }
  ], path.join(state.base, 'physical-duplicate-data'));
  await assert.rejects(
    createToolContext(physicalDuplicateConfig),
    error => error?.code === 'WORKSPACE_FOLDER_DUPLICATE_ROOT'
  );

  const firstContext = await createToolContext(config(state.folders, path.join(state.base, 'restart-a')));
  const secondContext = await createToolContext(config(state.folders, path.join(state.base, 'restart-b')));
  assert.equal(firstContext.workspaceProfileId, secondContext.workspaceProfileId);
  await firstContext.conversations.flush();
  await secondContext.conversations.flush();
  await firstContext.usageStore.flush();
  await secondContext.usageStore.flush();
  assert.match(firstContext.workspaceProfileId, /^node-[0-9a-f]{16}$/);
});
