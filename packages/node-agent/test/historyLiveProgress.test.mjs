import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  beginSessionActivity,
  sessionRecentActions,
} from '../dist/sessionActivity.js';
import { renderDocument } from '../dist/historyMarkdown.js';
import {
  managementHistoryBackgroundJobsPayload,
  managementHistoryDetailPayload,
  managementHistoryListPayload,
} from '../dist/managementObservability.js';
import { historyDetailNeedsRefresh } from '../../../src/lib/history/live-detail.ts';

test('a live conversation shows sanitized tool progress, completion status and a bounded recent timeline', () => {
  const owner = {};
  const first = beginSessionActivity(owner, 'conversation-A', 'exec_command',
    { program: 'cargo', path: '.ssh/id_rsa' }, 1000);
  assert.ok(first);
  assert.deepEqual(sessionRecentActions(owner, 'conversation-A').map(item => item.status), ['running']);
  assert.doesNotMatch(JSON.stringify(sessionRecentActions(owner, 'conversation-A')), /id_rsa/);
  first.complete('success', 1250);
  const second = beginSessionActivity(owner, 'conversation-A', 'read_file',
    { path: 'src/lib.rs' }, 1300);
  assert.ok(second);
  second.complete('failed', 1340);
  const history = sessionRecentActions(owner, 'conversation-A');
  assert.equal(history.length, 2);
  assert.equal(history[0].tool, 'read_file');
  assert.equal(history[0].status, 'failed');
  assert.equal(history[1].status, 'success');
  assert.equal(history[0].updatedAtMs, 1340);
  assert.deepEqual(sessionRecentActions(owner, 'conversation-B'), []);
  assert.deepEqual(sessionRecentActions({}, 'conversation-A'), []);
  second.complete('success', 1550);
  assert.equal(sessionRecentActions(owner, 'conversation-A')[0].status, 'failed');

  for (let i = 0; i < 25; i++) {
    beginSessionActivity(owner, 'conversation-A', 'search_text', { query: 'test' }, 2000 + i)?.complete('success', 2100 + i);
  }
  assert.equal(sessionRecentActions(owner, 'conversation-A').length, 16);
  assert.ok(sessionRecentActions(owner, 'conversation-A').every(item => item.action.length <= 120));
});

test('the selected history file changes by SHA revision without requiring a new Updated timestamp', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-live-history-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const dir = path.join(root, 'docs', 'history-session');
  await mkdir(dir, { recursive: true });
  const owner = {};
  const ctx = { conversations: owner, config: { folders: [{ id: 'repo', name: 'Repo', path: root }] } };
  const entry = notes => ({
    turn_id: 'turn-1', timestamp: 'unix:2', user_intent: 'Update live view',
    findings: [], decisions: [], files_changed: [], tests: [],
    runtime_state: [], remaining_issues: [], next_actions: [], notes,
  });
  const write = notes => writeFile(path.join(dir, '1.md'),
    renderDocument(1, 'Live test', 'owned-session', 'unix:1', 'unix:2', 'active', [entry(notes)]));
  await write('The original note.');
  const beforeList = await managementHistoryListPayload(ctx, 'repo');
  const beforeDetail = await managementHistoryDetailPayload(ctx, 1, 'repo');
  const before = beforeList.sessions[0];
  assert.equal(before.revision, beforeDetail.revision);
  assert.equal(historyDetailNeedsRefresh(beforeDetail, before), false);

  await write('The latest test actually passed.');
  const afterList = await managementHistoryListPayload(ctx, 'repo');
  const afterDetail = await managementHistoryDetailPayload(ctx, 1, 'repo');
  const after = afterList.sessions[0];
  assert.equal(after.checkpointCount, before.checkpointCount);
  assert.equal(after.updatedAt, before.updatedAt);
  assert.equal(after.status, before.status);
  assert.notEqual(after.revision, before.revision);
  assert.equal(historyDetailNeedsRefresh(beforeDetail, after), true);
  assert.equal(historyDetailNeedsRefresh(afterDetail, after), false);
  assert.match(afterDetail.content, /latest test actually passed/);
  assert.doesNotMatch(JSON.stringify(after), /owned-session/);

  const task = beginSessionActivity(owner, 'owned-session', 'exec_command', { program: 'cargo' });
  task?.complete('success');
  const activity = await managementHistoryBackgroundJobsPayload(ctx, 1, 'repo');
  assert.equal(activity.jobs.length, 0);
  assert.equal(activity.recentActions.length, 1);
  assert.equal(activity.recentActions[0].status, 'success');
  assert.doesNotMatch(JSON.stringify(activity), /owned-session/);
  const afterToolList = await managementHistoryListPayload(ctx, 'repo');
  assert.equal(afterToolList.sessions[0].revision, after.revision);
  assert.equal(afterToolList.sessions[0].recentActions[0].tool, 'exec_command');
  assert.equal(historyDetailNeedsRefresh(afterDetail, afterToolList.sessions[0]), false);
});

test('legacy backends without content revision still refresh on actual checkpoint metadata changes', () => {
  const latest = { revision: '', updatedAt:'unix:2', status:'active', checkpointCount:2, summary:'New note', title:'Session' };
  const current = { revision:'', updatedAt:'unix:1', status:'active', records:[{}], summary:'Old note', title:'Session' };
  assert.equal(historyDetailNeedsRefresh(current, latest), true);
  assert.equal(historyDetailNeedsRefresh(null, latest), false);
});

test('session HistoryViewer keeps existing detail mounted and reloads it silently only after a change', async () => {
  const page = await readFile(new URL('../../../src/lib/components/HistoryViewer.svelte', import.meta.url),'utf8');
  const panel = await readFile(new URL('../../../src/lib/components/HistoryBackgroundJobs.svelte', import.meta.url),'utf8');
  assert.match(page, /historyDetailNeedsRefresh\(detail, session\)/);
  assert.match(page, /void refreshSelectedDetail\(current\)/);
  assert.match(page, /selectedSequence !== detailSequence \|\| session.key !== selectedKey/);
  assert.match(page, /detail = latest/);
  assert.match(panel, /liveRecentActions = result.recentActions \?\? \[\]/);
  assert.match(panel, /liveRecentActions\.length \? liveRecentActions : fallbackRecentActions/);
  assert.match(panel, /window.setInterval\(/);
});
