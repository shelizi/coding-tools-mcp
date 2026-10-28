import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ConversationStore } from '../dist/conversation.js';
import { listActiveWorkspaces, noteWorkspaceToolAccess } from '../dist/activeWorkspaceAccess.js';

test('lists active and recent MCP workspace/worktree usage without exposing conversation IDs', t => {
  const root = mkdtempSync(path.join(tmpdir(), 'ctmcp-active-workspaces-'));
  t.after(() => rmSync(root, { force: true, recursive: true }));
  mkdirSync(path.join(root, '.git'));
  const worktree = path.join(root, 'worktrees', 'feature');
  mkdirSync(worktree, { recursive: true });
  writeFileSync(path.join(worktree, '.git'), 'gitdir: ../../.git/worktrees/feature');
  const ctx = {
    conversations: new ConversationStore(),
    config: { folders: [{ id: 'repo', name: 'Repository', path: root }] }
  };
  let now = 1_800_000_000_000;
  t.mock.method(Date, 'now', () => now);

  assert.equal(listActiveWorkspaces(ctx, {}).active_project_count, 0);
  noteWorkspaceToolAccess(ctx, 'conversation-A', 'repo', 'read_file', { path: 'worktrees/feature' });
  noteWorkspaceToolAccess(ctx, 'conversation-A', 'repo', 'read_file', { path: 'worktrees/feature' });
  noteWorkspaceToolAccess(ctx, 'conversation-B', 'repo', 'git_status', { path: 'worktrees/feature' });
  noteWorkspaceToolAccess(ctx, 'conversation-C', 'repo', 'git_status', { path: '.' });

  let listing = listActiveWorkspaces(ctx, {});
  assert.equal(listing.active_project_count, 1);
  assert.equal(listing.active_worktree_count, 1);
  assert.equal(listing.projects[0].conversation_count, 3);
  const tree = listing.projects[0].checkouts.find(item => item.kind === 'worktree');
  assert.equal(tree.conversation_count, 2);
  assert.equal(tree.tool_call_count, 3);
  assert.equal(tree.status, 'active');
  assert.ok(!JSON.stringify(listing).includes('conversation-A'));

  now += 4 * 60_000;
  listing = listActiveWorkspaces(ctx, {});
  assert.equal(listing.active_project_count, 0);
  assert.equal(listing.recent_project_count, 1);
  assert.equal(listing.projects[0].status, 'recent');
  assert.equal(listing.projects[0].checkouts.find(item => item.kind === 'worktree').status, 'recent');

  now += 2 * 60_000;
  listing = listActiveWorkspaces(ctx, {});
  assert.equal(listing.active_project_count, 0);
  assert.equal(listing.recent_project_count, 0);
  assert.deepEqual(listing.projects, []);
});

test('rejects invalid time windows and ignores configured but untouched workspaces', () => {
  const ctx = { conversations: new ConversationStore(), config: { folders: [{ id: 'unused', path: 'Z:/missing', name: 'Unused' }] } };
  assert.equal(listActiveWorkspaces(ctx, {}).active_project_count, 0);
  assert.equal(listActiveWorkspaces(ctx, { active_within_minutes: 0 }).ok, false);
  assert.equal(listActiveWorkspaces(ctx, { active_within_minutes: 6, recent_within_minutes: 5 }).ok, false);
  assert.equal(listActiveWorkspaces(ctx, { active_within_minutes: 3.5 }).ok, false);
});

test('does not mistake a submodule gitdir for a linked worktree', t => {
  const root = mkdtempSync(path.join(tmpdir(), 'ctmcp-submodule-'));
  t.after(() => rmSync(root, { force: true, recursive: true }));
  mkdirSync(path.join(root, '.git'));
  const submodule = path.join(root, 'modules', 'nested');
  mkdirSync(submodule, { recursive: true });
  writeFileSync(path.join(submodule, '.git'), 'gitdir: ../../.git/modules/nested');
  const ctx = {
    conversations: new ConversationStore(),
    config: { folders: [{ id: 'repo', name: 'Repository', path: root }] }
  };
  noteWorkspaceToolAccess(ctx, 'conversation-sub', 'repo', 'read_file', { path: 'modules/nested' });
  const listing = listActiveWorkspaces(ctx, {});
  assert.equal(listing.active_worktree_count, 0);
  assert.equal(listing.projects[0].checkouts[0].kind, 'repository');
});
