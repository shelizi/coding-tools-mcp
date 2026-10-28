import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import {
  collectHistory,
  historyLocationKey,
  historySessionKey,
  sortLocatedHistory,
  uniqueHistoryLocations,
} from '../../../src/lib/history/all-projects.ts';

function location(workspaceId, folderId, folderPath) {
  return {
    key: historyLocationKey(workspaceId, folderId),
    workspaceId,
    workspaceName: `project-${workspaceId}`,
    folderId,
    folderName: `folder-${folderId}`,
    folderPath,
  };
}

function session(number, options = {}) {
  return {
    number,
    path: `docs/history-session/${number}.md`,
    title: `session ${number}`,
    sessionKey: null,
    createdAt: null,
    updatedAt: '2026-10-23T00:00:00.000Z',
    status: 'completed',
    activityStatus: 'completed',
    activityTool: null,
    activityDescription: null,
    lastActivityAtMs: null,
    activeRequestCount: 0,
    lastActivityOutcome: null,
    summary: '',
    checkpointCount: 1,
    ...options,
  };
}

test('history keys prevent collisions between duplicate session numbers in different projects', async () => {
  const a = location('workspace-1', 'folder-1', 'E:/projects/alpha');
  const b = location('workspace-2', 'folder-2', 'E:/projects/beta');
  const result = await collectHistory([a, b], async () => ({
    historyDir: 'docs/history-session',
    sessions: [session(12)],
    count: 1,
    missingNumbers: [],
    invalidFiles: [],
    emptyFiles: [],
  }));
  assert.equal(result.succeeded, 2);
  assert.equal(result.failed.length, 0);
  assert.equal(result.sessions.length, 2);
  assert.equal(new Set(result.sessions.map(item => item.key)).size, 2);
  assert.deepEqual(new Set(result.sessions.map(item => item.workspaceId)), new Set(['workspace-1','workspace-2']));
  for (const item of result.sessions) {
    assert.equal(item.key, historySessionKey(item.workspaceId, item.folderId, 12));
    assert.equal(item.folderPath, item.workspaceId === 'workspace-1' ? a.folderPath : b.folderPath);
  }
  assert.notEqual(historySessionKey('a:b', 'c', 1), historySessionKey('a', 'b:c', 1));
});

test('all-projects history ignores duplicate workspace configurations pointing to the same folder', () => {
  const own = location('own', '1', 'E:\\Projects\\Alpha');
  const duplicate = location('other', '2', 'e:/projects/alpha/');
  const unique = location('third', '3', 'E:/projects/beta');
  assert.deepEqual(uniqueHistoryLocations([own, duplicate, unique]), [own, unique]);
  const posixA = location('wsl-a', '1', '/home/CaseSensitive');
  const posixB = location('wsl-b', '2', '/home/casesensitive');
  assert.deepEqual(uniqueHistoryLocations([posixA, posixB]), [posixA, posixB]);
});

test('all-project history collection bounds concurrent reads and keeps successful projects when one fails', async () => {
  const sources = Array.from({length: 11}, (_, index) =>
    location(`workspace-${index}`, `folder-${index}`, `E:/repo/${index}`));
  let active = 0;
  let highest = 0;
  const reads = [];
  const result = await collectHistory(sources, async (workspaceId, folderId) => {
    active++;
    highest = Math.max(highest, active);
    reads.push([workspaceId,folderId]);
    try {
      await new Promise(resolve => setTimeout(resolve, 3));
      if (workspaceId === 'workspace-4') throw new Error('unavailable');
      return {
        historyDir: '',
        sessions: [session(1, {
          activityStatus: workspaceId === 'workspace-2' ? 'running' : 'completed',
          lastActivityAtMs: workspaceId === 'workspace-2' ? 1000 : null,
        })],
        count: 1, missingNumbers: [], invalidFiles: [], emptyFiles: [],
      };
    } finally { active--; }
  }, 3);
  assert.ok(highest <= 3 && highest >= 2, `max concurrency ${highest}`);
  assert.equal(reads.length, 11);
  assert.equal(result.succeeded, 10);
  assert.deepEqual(result.failed.map(item => item.workspaceId), ['workspace-4']);
  assert.equal(result.sessions.length, 10);
  assert.equal(result.sessions[0].workspaceId, 'workspace-2');
});

test('cancelling all-project loading stops scheduling requests for other folders', async () => {
  const locations = Array.from({length: 20}, (_, index) =>
    location(`w${index}`, `f${index}`, `E:/repo/${index}`));
  const controller = new AbortController();
  let fetched = 0;
  let release;
  const hold = new Promise(resolve => { release = resolve; });
  const pending = collectHistory(locations, async () => {
    fetched++;
    await hold;
    throw new Error('cancelled request');
  }, 3, controller.signal);
  controller.abort();
  release();
  const result = await pending;
  assert.ok(fetched <= 3, `started ${fetched} requests after abort`);
  assert.equal(result.failed.length, 0);
  assert.equal(result.sessions.length, 0);
});

test('sorted history prioritizes active status then newest activity and stable ties', () => {
  const a = location('a','1','/a');
  const sessions = [
    { ...session(1,{updatedAt:'2025-01-01T00:00:00Z'}), ...a, key:historySessionKey('a','1',1) },
    { ...session(2,{updatedAt:'2026-10-23T00:00:00Z'}), ...a, key:historySessionKey('a','1',2) },
    { ...session(3,{activityStatus:'active', lastActivityAtMs:5}), ...a, key:historySessionKey('a','1',3) },
  ];
  assert.deepEqual(sortLocatedHistory(sessions).map(item=>item.number),[3,2,1]);
  assert.deepEqual(sessions.map(item=>item.number),[1,2,3]); // input is not mutated
});

test('all-projects HistoryViewer chooses true workspace/folder for details and provides filtering', async () => {
  const source = await readFile(new URL('../../../src/lib/components/HistoryViewer.svelte',import.meta.url),'utf8');
  assert.match(source,/value="all">\{\$t\("All projects"\)\}/);
  assert.match(source,/readHistorySession\(session\.workspaceId, session\.number, session\.folderId\)/);
  assert.match(source,/session\.key/);
  // The history refresh cadence is adaptive (3/6/12/15s), not the retired 30s timer.
  assert.match(source,/historyPollCadenceMs\(selectedScope, sources\.length\)/);
  assert.match(source,/3_000/);
  assert.match(source,/collectHistory\(/);
  assert.match(source,/request\.signal/);
});
