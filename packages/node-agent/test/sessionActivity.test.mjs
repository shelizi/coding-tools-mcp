import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

import {
  ACTIVE_WINDOW_MS,
  beginSessionActivity,
  sessionActivitySnapshot,
  trackMcpToolActivity,
} from '../dist/sessionActivity.js';
import { renderDocument } from '../dist/historyMarkdown.js';
import { managementHistoryListPayload } from '../dist/managementObservability.js';

test('Node reports Rust-equivalent running, recently active and inactive after 5 minutes', () => {
  const owner = {};
  const guard = beginSessionActivity(owner, 'native-session', 'read_file', { path: 'src/main.ts' }, 1_000);
  assert.ok(guard);
  assert.deepEqual(
    { ...sessionActivitySnapshot(owner, 'native-session', 1_001) },
    {
      status: 'running', tool: 'read_file', action: 'read_file · src/main.ts',
      lastActivityAtMs: 1_000, activeRequestCount: 1, lastOutcome: '',
    },
  );
  guard.complete('success', 2_000);
  assert.equal(sessionActivitySnapshot(owner, 'native-session', 2_000 + ACTIVE_WINDOW_MS)?.status, 'active');
  assert.equal(sessionActivitySnapshot(owner, 'native-session', 2_001 + ACTIVE_WINDOW_MS)?.status, 'inactive');
  assert.equal(sessionActivitySnapshot(owner, 'native-session', 2_100)?.activeRequestCount, 0);
  assert.equal(sessionActivitySnapshot(owner, 'native-session', 2_100)?.lastOutcome, 'success');
});

test('parallel Node requests keep latest unfinished action without mixing workspaces', () => {
  const owner = {}, otherOwner = {};
  const first = beginSessionActivity(owner, 'session', 'read_file', { path: 'a.ts' }, 100);
  const second = beginSessionActivity(owner, 'session', 'search_text', { query: 'TODO' }, 110);
  assert.ok(first && second);
  assert.equal(sessionActivitySnapshot(owner, 'session', 120)?.action, 'search_text · TODO');
  assert.equal(sessionActivitySnapshot(owner, 'session', 120)?.activeRequestCount, 2);
  assert.equal(sessionActivitySnapshot(otherOwner, 'session'), undefined);
  second.complete('success', 140);
  assert.equal(sessionActivitySnapshot(owner, 'session', 150)?.action, 'read_file · a.ts');
  first.complete('cancelled', 160);
  first.complete('success', 300);
  assert.equal(sessionActivitySnapshot(owner, 'session', 300)?.lastActivityAtMs, 160);
  assert.equal(sessionActivitySnapshot(owner, 'session', 300)?.lastOutcome, 'cancelled');
  assert.equal(beginSessionActivity(owner, undefined, 'read_file', {path:'x'}), undefined);
  assert.equal(beginSessionActivity(owner, '', 'read_file', {path:'x'}), undefined);
});

test('Node action summary redacts sensitive path and caps text like Rust', async () => {
  const owner = {};
  const pathGuard = beginSessionActivity(owner, 'session-1', 'read_file', { path: '.ssh/id_rsa' }, 10);
  assert.ok(pathGuard);
  assert.doesNotMatch(sessionActivitySnapshot(owner, 'session-1', 11)?.action ?? '', /id_rsa/i);
  pathGuard.complete('success', 12);
  const many = beginSessionActivity(owner, 'session-2', 'exec_many', { commands: [{}, {}, {}] }, 10);
  assert.equal(sessionActivitySnapshot(owner, 'session-2', 12)?.action, 'exec_many · 3 commands');
  many?.complete('success', 12);
  const long = beginSessionActivity(owner, 'session-3', 'read_file', { path: 'x'.repeat(500) }, 10);
  assert.ok((sessionActivitySnapshot(owner, 'session-3', 12)?.action.length ?? 0) <= 120);
  long?.complete('success', 12);
  await assert.rejects(() => trackMcpToolActivity(owner, 'error', 'read_file', {path:'x'}, async () => {
    throw new Error('tool failed');
  }));
  assert.equal(sessionActivitySnapshot(owner, 'error')?.lastOutcome, 'failed');
});

test('Node management history returns activity metadata and respects completed documents without leaking keys', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-node-history-activity-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const dir = path.join(root, 'docs', 'history-session');
  await mkdir(dir, {recursive:true});
  for (const [number,key,status] of [
    [1,'native-ongoing','active'], [2,'native-stale','active'], [3,'native-finished','completed'], [4,'not-tracked','active'],
  ]) {
    await writeFile(path.join(dir, `${number}.md`), renderDocument(number, `Session ${number}`, key, 'unix:1', 'unix:2', status, []));
  }
  const owner={};
  const context={conversations:owner,config:{folders:[{id:'repo',name:'Repo',path:root}]}};
  const now=Date.now();
  const running=beginSessionActivity(owner, 'native-ongoing', 'search_text', {query:'issue'}, now);
  const stale=beginSessionActivity(owner, 'native-stale', 'read_file', {path:'older.txt'}, now-ACTIVE_WINDOW_MS-3000);
  assert.ok(running && stale);
  stale.complete('success', now-ACTIVE_WINDOW_MS-2000);
  const finished=beginSessionActivity(owner, 'native-finished', 'read_file', {path:'done.txt'}, now);
  assert.ok(finished);

  const list=await managementHistoryListPayload(context,'repo');
  const byNum=new Map(list.sessions.map(item=>[item.number,item]));
  assert.equal(byNum.get(1).activityStatus,'running');
  assert.equal(byNum.get(1).activeRequestCount,1);
  assert.equal(byNum.get(1).activityDescription,'search_text · issue');
  assert.equal(byNum.get(2).activityStatus,'inactive');
  assert.equal(byNum.get(2).lastActivityOutcome,'success');
  assert.equal(byNum.get(3).activityStatus,'completed');
  assert.equal(byNum.get(4).activityStatus,'inactive');
  assert.equal(byNum.get(4).lastActivityAtMs,null);
  assert.doesNotMatch(JSON.stringify(list),/native-ongoing|native-stale|native-finished|not-tracked/);
  running.complete('success');
  finished.complete('success');
  const activeList=await managementHistoryListPayload(context,'repo');
  assert.equal(activeList.sessions.find(item=>item.number===1).activityStatus,'active');
});

test('History viewer renders newest checkpoint first and retains original left session sorting', async () => {
  const { readFile }=await import('node:fs/promises');
  const history=await readFile(new URL('../../../src/lib/components/HistoryViewer.svelte',import.meta.url),'utf8');
  const sorter=await readFile(new URL('../../../src/lib/history/all-projects.ts',import.meta.url),'utf8');
  assert.match(history,/\[\.\.\.detail\.records\]\.reverse\(\)/);
  assert.match(history,/\{#each displayedRecords as record/);
  assert.match(sorter,/priority\[left\.activityStatus\] - priority\[right\.activityStatus\]/);
  assert.match(history,/\{#each displayedSessions as session/);
});
