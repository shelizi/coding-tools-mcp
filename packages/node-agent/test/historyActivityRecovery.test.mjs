import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { renderDocument } from '../dist/historyMarkdown.js';
import { mergeAutoActivity, recentPersistedActivity } from '../dist/historyAutoMarkdown.js';
import { BACKGROUND_HEARTBEAT_MS, backgroundObservationId } from '../dist/historyAutoPersist.js';
import { managementHistoryListPayload } from '../dist/managementObservability.js';
import {
  ACTIVE_WINDOW_MS,
  beginSessionActivity,
} from '../dist/sessionActivity.js';

const now = () => Date.now();
const observation = (id, atMs, status = 'success', action = 'read_file · src/main.ts') => ({
  id, atMs, tool: 'read_file', action, status,
});

async function createArchive(t, entries) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-node-activity-recovery-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const dir = path.join(root, 'docs', 'history-session');
  await mkdir(dir, { recursive: true });
  for (const [number, key, content] of entries) {
    await writeFile(path.join(dir, `${number}.md`),
      content ?? renderDocument(number, `Test ${number}`, key, 'unix:1', 'unix:2', 'active', []));
  }
  const owner = {};
  const ctx = {
    conversations: owner,
    config: { folders: [{ id: 'folder1', name: 'Folder1', path: root }] },
    folderRuntimes: new Map(),
  };
  const list = () => managementHistoryListPayload(ctx, 'folder1');
  return { root, dir, owner, ctx, list };
}

test('Node really active 4 minutes ago remains recently active, not falsely running', async t => {
  const archive = await createArchive(t, [[1, 'native-real']]);
  const at = now() - 4 * 60_000;
  const event = beginSessionActivity(archive.owner, 'native-real', 'read_file',
    { path: 'src/main.ts' }, at);
  event?.complete('success', at + 100);
  const result = await archive.list();
  const item = result.sessions[0];
  assert.equal(ACTIVE_WINDOW_MS, 5 * 60_000);
  assert.equal(item.activityStatus, 'active');
  assert.equal(item.activeRequestCount, 0);
  assert.equal(item.lastActivityAtMs, at + 100);
});

test('Node restores real recent tool activity from its own Markdown after restart', async t => {
  const at = now() - 3 * 60_000;
  const key = 'native-restarted';
  const content = mergeAutoActivity(
    renderDocument(1, 'Restart', key, 'unix:1', 'unix:2', 'active', []),
    [observation('node:123:111:1:running', at - 150, 'running'),
     observation('node:123:111:1:success', at, 'success')],
  );
  const archive = await createArchive(t, [[1, key, content]]);
  // No call to beginSessionActivity: runtime process has lost all in-memory
  // activity while the trusted bounded Markdown activity was persisted.
  const result = await archive.list();
  const item = result.sessions[0];
  assert.equal(item.activityStatus, 'active');
  assert.equal(item.activeRequestCount, 0);
  assert.equal(item.lastActivityAtMs, at);
  assert.equal(item.activityTool, 'read_file');
  assert.equal(item.activityDescription, 'read_file · src/main.ts');
  assert.equal(item.recentActions.length, 1);
  assert.equal(item.recentActions[0].status, 'success');
  assert.doesNotMatch(JSON.stringify(item), /native-restarted/);
});

test('Rust-generated unix timestamps also restore Node recent activity', async t => {
  const at = now() - 2 * 60_000;
  const content = mergeAutoActivity(
    renderDocument(1, 'From Rust', 'rust-archive', 'unix:1', 'unix:2', 'active', []),
    [observation('node:1:2:3:success', at)],
  ).replace(new Date(at).toISOString(), `unix:${(at / 1000).toFixed(3)}`)
    .replace('ctmcp-activity:node:1:2:3:success', 'ctmcp-activity:rust:1:2:3:success');
  const archive = await createArchive(t, [[1, 'rust-archive', content]]);
  const item = (await archive.list()).sessions[0];
  assert.equal(item.activityStatus, 'active');
  assert.equal(item.recentActions[0].status, 'success');
  assert.equal(item.lastActivityAtMs, at);
});

test('Node prefers newer Rust persisted activity over older local in-memory timeline', async t => {
  const at = now() - 45_000;
  const localAt = now() - 4 * 60_000;
  const key = 'cross-runtime-fresher';
  const content = mergeAutoActivity(
    renderDocument(1, 'Cross runtime', key, 'unix:1', 'unix:2', 'active', []),
    [observation('rust:9:111:1:success', at, 'success', 'read_file · src/main.ts')],
  ).replace(new Date(at).toISOString(), `unix:${(at / 1000).toFixed(3)}`);
  const archive = await createArchive(t, [[1, key, content]]);
  const local = beginSessionActivity(archive.owner, key, 'search_text', { query: 'old' }, localAt);
  local?.complete('success', localAt + 50);
  const item = (await archive.list()).sessions[0];
  assert.equal(item.activityStatus, 'active');
  assert.equal(item.activeRequestCount, 0);
  assert.equal(item.lastActivityAtMs, at);
  assert.equal(item.activityTool, 'read_file');
  assert.equal(item.recentActions[0].updatedAtMs, at);
  assert.equal(item.recentActions[0].status, 'success');
  assert.doesNotMatch(item.recentActions[0].action, /old/);
});

test('saved in-flight background job is observed, NEVER resurrected as running', async t => {
  const at = now() - 2 * 60_000;
  const content = mergeAutoActivity(
    renderDocument(1, 'Background', 'background-restart', 'unix:1', 'unix:2', 'active', []),
    [observation('node:123:abcdef:background_running', at, 'background_running')],
  );
  const archive = await createArchive(t, [[1, 'background-restart', content]]);
  const item = (await archive.list()).sessions[0];
  assert.equal(item.activityStatus, 'active');
  assert.equal(item.activeRequestCount, 0);
  assert.equal(item.recentActions[0].status, 'observed');
});

test('real long-running background heartbeats keep other runtimes active without inventing a live process', async t => {
  const observedAt = now() - 10 * 60_000;
  const recentAt = now() - 25_000;
  const digest = 'abcdef0123456789';
  const initial = backgroundObservationId(123, digest, true, observedAt);
  const renewed = backgroundObservationId(123, digest, true, recentAt);
  assert.notEqual(initial, renewed);
  const currentMinute = Math.floor(recentAt / BACKGROUND_HEARTBEAT_MS) * BACKGROUND_HEARTBEAT_MS;
  assert.equal(renewed, backgroundObservationId(123, digest, true, currentMinute + 9_000));
  assert.equal(BACKGROUND_HEARTBEAT_MS, 60_000);
  const content = mergeAutoActivity(
    renderDocument(1, 'Verified heartbeat', 'heartbeat-owner', 'unix:1', 'unix:2', 'active', []),
    [observation(initial, observedAt, 'background_running'),
     observation(renewed, recentAt, 'background_running')],
  );
  const archive = await createArchive(t, [[1, 'heartbeat-owner', content]]);
  const activity = (await archive.list()).sessions[0];
  assert.equal(activity.activityStatus, 'active');
  assert.equal(activity.activeRequestCount, 0);
  assert.equal(activity.lastActivityAtMs, recentAt);
  assert.equal(activity.recentActions[0].status, 'observed');
  const oldArchive = await createArchive(t, [[1, 'old-heartbeat', mergeAutoActivity(
    renderDocument(1, 'Expired heartbeat', 'old-heartbeat', 'unix:1', 'unix:2', 'active', []),
    [observation(initial, observedAt, 'background_running')],
  )]]);
  assert.equal((await oldArchive.list()).sessions[0].activityStatus, 'inactive');
  assert.equal(
    backgroundObservationId(123, digest, false, now()),
    backgroundObservationId(123, digest, false, now() + 3_600_000),
  );
});

test('expired, unrecorded and future activity cannot make an old Node session active', async t => {
  const old = now() - ACTIVE_WINDOW_MS - 1000;
  const future = now() + 3_600_000;
  const key = 'gone';
  const oldContent = mergeAutoActivity(
    renderDocument(1, 'Old', key, 'unix:1', 'unix:2', 'active', []),
    [observation('node:123:111:1:success', old)],
  );
  const futureContent = mergeAutoActivity(
    renderDocument(2, 'Future', 'future', 'unix:1', 'unix:2', 'active', []),
    [observation('node:123:222:1:success', future)],
  );
  const archive = await createArchive(t, [
    [1, key, oldContent], [2, 'future', futureContent], [3, 'none'],
  ]);
  const list = await archive.list();
  assert.ok(list.sessions.every(item => item.activityStatus === 'inactive'));
  assert.ok(list.sessions.every(item => item.recentActions.length === 0));
  assert.equal(recentPersistedActivity(oldContent).length, 0);
  assert.equal(recentPersistedActivity(futureContent).length, 0);
});

test('verified tool activity older than five minutes remains inactive (no phantom active projects)', async t => {
  const observedAt = now() - 11 * 60_000;
  const archived = mergeAutoActivity(
    renderDocument(1, 'Old source', 'old-real', 'unix:1', 'unix:2', 'active', []),
    [observation('node:42:101:1:success', observedAt)],
  );
  const { list, owner } = await createArchive(t, [[1, 'old-real', archived]]);
  assert.equal((await list()).sessions[0].activityStatus, 'inactive');
  const runtime = beginSessionActivity(owner, 'old-real', 'read_file',
    {path:'src/main.ts'}, observedAt);
  runtime?.complete('success', observedAt + 1);
  assert.equal((await list()).sessions[0].activityStatus, 'inactive');
});

test('malformed or duplicate saved tool events fail safely without fabricating activity', async () => {
  const at = now() - 2 * 60_000;
  const original = renderDocument(1, 'Safe', 'owner', 'unix:1', 'unix:2', 'active', []);
  const content = mergeAutoActivity(original, [
    observation('node:99:12:5:running', at - 100, 'running'),
    observation('node:99:12:5:success', at, 'success'),
  ]);
  const parsed = recentPersistedActivity(content);
  assert.equal(parsed.length, 1);
  assert.equal(parsed[0].status, 'success');
  assert.equal(parsed[0].updatedAtMs, at);
  const broken = content.replace('<!-- coding-tools-mcp:auto-activity:end -->', '');
  assert.deepEqual(recentPersistedActivity(broken), []);
  assert.deepEqual(recentPersistedActivity(original), []);
});

test('both History and Overview keep verified persisted activity when the live jobs endpoint returns an empty timeline', async () => {
  const { readFile } = await import('node:fs/promises');
  const history = await readFile(new URL('../../../src/lib/components/HistoryBackgroundJobs.svelte', import.meta.url), 'utf8');
  const overview = await readFile(new URL('../../../src/lib/history/active-projects.ts', import.meta.url), 'utf8');
  assert.match(history, /liveRecentActions\.length \? liveRecentActions : fallbackRecentActions/);
  assert.match(overview, /snapshot\?\.recentActions\?\.length/);
});
