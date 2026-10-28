import test from 'node:test';
import assert from 'node:assert/strict';
import {
  ACTIVE_WINDOW_MS,
  beginSessionActivity,
  observeManagedProcesses,
  sessionActivitySnapshot,
  trackMcpToolActivity,
} from '../dist/sessionActivity.js';
import { historyPollCadenceMs } from '../../../src/lib/history/polling.ts';

test('a managed Node command stays running after the MCP response until the real process exits', async () => {
  const owner = {};
  const reply = await trackMcpToolActivity(owner, 'active-session', 'exec_command', { program: 'cargo' }, async () => ({
    structuredContent: { ok: true, status: 'running', session_id: 'p1' },
    isError: false,
  }));
  assert.equal(reply.structuredContent.session_id, 'p1');
  const now = Date.now();
  const running = sessionActivitySnapshot(owner, 'active-session', now + ACTIVE_WINDOW_MS * 3, () => ({running:true}));
  assert.equal(running.status, 'running');
  assert.equal(running.activeRequestCount, 1);
  assert.equal(running.tool, 'exec_command');
  const doneAt = Date.now();
  const finished = sessionActivitySnapshot(owner, 'active-session', doneAt, () => ({
    running: false, endedAtMs: doneAt, outcome: 'success'
  }));
  assert.equal(finished.status, 'active');
  assert.equal(finished.lastActivityAtMs, doneAt);
  assert.equal(finished.activeRequestCount, 0);
  assert.equal(sessionActivitySnapshot(owner,'active-session',doneAt+ACTIVE_WINDOW_MS+1)?.status,'inactive');
});

test('Node tracks concurrent managed batch jobs and safely removes them when completed', async () => {
  const owner={};
  await trackMcpToolActivity(owner, 'batch-session', 'exec_many', { commands:[{},{}] }, async () => ({
    structuredContent: { ok:true, results:[
      { id:'a', result: {status:'running',session_id:'p1'} },
      { id:'b', result: {status:'verifying',session_id:'p2'} }
    ]}
  }));
  assert.equal(sessionActivitySnapshot(owner,'batch-session',Date.now(),()=>({running:true})).activeRequestCount,2);
  assert.equal(sessionActivitySnapshot(owner,'batch-session',Date.now(),id=>({running:id==='p2'})).activeRequestCount,1);
  assert.equal(sessionActivitySnapshot(owner,'batch-session',Date.now(),()=>({running:false})).activeRequestCount,0);
});

test('external MCP responses cannot forge a local managed background process', () => {
  const owner={};
  const guard=beginSessionActivity(owner,'session','external_browser',{},Date.now());
  assert.ok(guard);
  guard.complete('success');
  observeManagedProcesses(owner,'session','external_browser',
    {structuredContent:{status:'running',session_id:'made-up'}});
  const snapshot=sessionActivitySnapshot(owner,'session',Date.now(),()=>({running:true}));
  assert.equal(snapshot.status,'active');
  assert.equal(snapshot.activeRequestCount,0);
});

test('history global polling becomes faster while keeping a cost bound for many project folders', () => {
  assert.equal(historyPollCadenceMs('folder',100),3_000);
  assert.equal(historyPollCadenceMs('all',2),6_000);
  assert.equal(historyPollCadenceMs('all',7),12_000);
  assert.equal(historyPollCadenceMs('all',20),15_000);
});
