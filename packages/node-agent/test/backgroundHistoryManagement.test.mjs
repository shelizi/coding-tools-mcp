import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { beginSessionActivity, observeManagedProcesses } from '../dist/sessionActivity.js';
import { renderDocument } from '../dist/historyMarkdown.js';
import { managementHistoryListPayload } from '../dist/managementObservability.js';

test('Node management history reports real running background jobs and stops when the registry process exits', async t => {
  const root=await mkdtemp(path.join(tmpdir(),'ctmcp-bg-history-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const folder=path.join(root,'docs','history-session');
  await mkdir(folder,{recursive:true});
  await writeFile(path.join(folder,'1.md'),renderDocument(1,'Background command',
    'owner-session-key','unix:1','unix:2','active',[]));
  const owner={};
  const guard=beginSessionActivity(owner,'owner-session-key','exec_command',{program:'cargo'},Date.now());
  assert.ok(guard);
  guard.complete('success');
  observeManagedProcesses(owner,'owner-session-key','exec_command',
    {structuredContent:{status:'running',session_id:'background-001'}});
  const process={id:'background-001',endedAt:undefined,finalizedAt:undefined};
  const ctx={conversations:owner,config:{folders:[{id:'repo',name:'Repo',path:root}]},
    folderRuntimes:new Map([['repo',{sessions:new Map([['background-001',process]])}]])};
  const running=await managementHistoryListPayload(ctx,'repo');
  assert.equal(running.sessions[0].activityStatus,'running');
  assert.equal(running.sessions[0].activeRequestCount,1);
  process.endedAt=Date.now();
  process.finalizedAt=process.endedAt;
  process.exitCode=0;
  const done=await managementHistoryListPayload(ctx,'repo');
  assert.equal(done.sessions[0].activityStatus,'active');
  assert.equal(done.sessions[0].activeRequestCount,0);
  assert.doesNotMatch(JSON.stringify(done),/owner-session-key|background-001/);
});
