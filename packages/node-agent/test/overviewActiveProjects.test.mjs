import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { aggregateActiveProjects } from '../../../src/lib/history/active-projects.ts';
import { uniqueHistoryLocations } from '../../../src/lib/history/all-projects.ts';

const action = (sequence, updatedAtMs, status='success') => ({
  sequence, tool: 'exec_command', action: 'exec_command · cargo test',
  startedAtMs: updatedAtMs-100, updatedAtMs, status,
});
function session(workspaceId, folderId, number, activityStatus, lastActivityAtMs, extras={}) {
  return {
    key: JSON.stringify([workspaceId,folderId,number]),
    workspaceId, workspaceName: workspaceId,
    folderId, folderName: folderId, folderPath: `E:\\Projects\\${folderId}`,
    number, activityStatus, lastActivityAtMs,
    title: 'PRIVATE_SESSION_TITLE_NEVER_RENDER',
    summary: 'PRIVATE_SUMMARY_NEVER_RENDER',
    recentActions:[], activityDescription:null, activityTool:null,
    activeRequestCount: activityStatus==='running'?1:0,
    updatedAt: 'unix:100', status:'active', revision:'',
    path:'', createdAt:'unix:1', lastActivityOutcome:null, checkpointCount:1,
    ...extras,
  };
}
const job = (id, startedAtMs) => ({
  id, tool:'exec_command', action:'cargo build',
  status:'running', startedAtMs, elapsedMs:Date.now()-startedAtMs,
});

test('overview groups multiple sessions into ONE project with running precedence and sorted tool activity', () => {
  const a=session('ws1','project-a',1,'active',4_000,{recentActions:[action(1,4_000)]});
  const b=session('ws1','project-a',2,'running',9_000,{recentActions:[action(1,8_000,'running')]});
  const c=session('ws1','project-b',1,'active',7_000,{recentActions:[action(1,6_000)]});
  const idle=session('ws1','project-c',1,'inactive',100_000);
  const done=session('ws1','project-d',1,'completed',110_000);
  const result=aggregateActiveProjects([idle,c,done,a,b], {
    [b.key]:{jobs:[job('background-1',8_500)],recentActions:[action(1,9_100,'success')],capturedAtMs:10_000},
  });
  assert.equal(result.length,2);
  const projectA=result[0];
  assert.equal(projectA.folderId,'project-a');
  assert.equal(projectA.status,'running');
  assert.equal(projectA.sessions.length,2);
  assert.equal(projectA.actions.length,2);
  assert.equal(projectA.actions[0].updatedAtMs,9_100);
  assert.equal(projectA.actions[0].status,'success');
  assert.equal(projectA.jobs.length,1);
  assert.equal(projectA.jobs[0].session.key,b.key);
  assert.equal(result[1].folderId,'project-b');
  assert.equal(result[1].status,'active');
  assert.doesNotMatch(JSON.stringify(result.map(x => ({
    key:x.key, folderId:x.folderId, actions:x.actions, jobs:x.jobs.map(j=>j.action),
  }))),/PRIVATE_SESSION_TITLE_NEVER_RENDER|PRIVATE_SUMMARY_NEVER_RENDER/);
});

test('overview excludes every idle or completed project; a recently active project disappears on updated history list', () => {
  const active=session('ws1','coding',1,'active',15_000);
  const idle={...active,activityStatus:'inactive'};
  assert.deepEqual(aggregateActiveProjects([idle]),[]);
  assert.equal(aggregateActiveProjects([active]).length,1);
  assert.deepEqual(aggregateActiveProjects([{...active,activityStatus:'completed'}]),[]);
  const completing=session('ws1','still-building',4,'completed',10_000,{activeRequestCount:1});
  assert.equal(aggregateActiveProjects([completing])[0].status,'running');
  const two=session('ws1','other',2,'active',25_000);
  const ordered=aggregateActiveProjects([active,two]);
  assert.deepEqual(ordered.map(x=>x.folderId),['other','coding']);
});

test('overview merges physically identical projects across workspace profiles without losing active work',()=>{
  const x=session('workspace-a','folder-a',1,'running',9_000);
  const y=session('workspace-b','folder-a',1,'active',8_000);
  const samePhysical=aggregateActiveProjects([x,y]);
  assert.equal(samePhysical.length,1);
  assert.equal(samePhysical[0].status,'running');
  assert.equal(samePhysical[0].sessions.length,2);
  assert.equal(aggregateActiveProjects([x,session('workspace-b','folder-b',1,'active',8_000)]).length,2);
  const configured=[
    {key:'one',workspaceId:'ws1',folderId:'id1',workspaceName:'A',
      folderName:'Coding',folderPath:'E:\\Projects\\Coding'},
    {key:'two',workspaceId:'ws2',folderId:'id2',workspaceName:'B',
      folderName:'Coding',folderPath:'e:/projects/coding/'},
  ];
  assert.equal(uniqueHistoryLocations(configured).length,1);
});

test('overview joins concurrent jobs from different sessions and bounds recent tool events',()=>{
  const s1=session('ws','repo',1,'running',10_000);
  const s2=session('ws','repo',2,'active',9_000);
  const result=aggregateActiveProjects([s1,s2],{
    [s1.key]:{jobs:[job('p1',8_000)],recentActions:
      Array.from({length:14},(_,i)=>action(i,10_000-i)),capturedAtMs:1},
    [s2.key]:{jobs:[job('p2',9_000)],recentActions:[action(1,7_000)],capturedAtMs:1},
  });
  assert.equal(result.length,1);
  assert.equal(result[0].jobs.length,2);
  assert.deepEqual(result[0].jobs.map(j=>j.id),['p2','p1']);
  assert.equal(result[0].actions.length,6);
  assert.ok(result[0].actions.every(x=>x.key.includes('repo')));
  assert.equal(aggregateActiveProjects([s1],{},0)[0].actions.length,0);
});

test('the real Overview renders project cards only and never reads a session article or checkpoint',async()=>{
  const base=new URL('../../../src/lib/components/workspace/',import.meta.url);
  const overview=await readFile(new URL('WorkspaceOverview.svelte',base),'utf8');
  const panel=await readFile(new URL('ActiveProjectsOverview.svelte',base),'utf8');
  assert.match(overview,/<ActiveProjectsOverview \/>/);
  assert.match(panel,/aggregateActiveProjects\(sessions, snapshots\)/);
  assert.match(panel,/item\.activityStatus === "running" \|\| item\.activityStatus === "active"/);
  assert.match(panel,/historyPollCadenceMs\("all", sources\.length\)/);
  assert.match(panel,/listHistoryBackgroundJobs\(/);
  assert.match(panel,/readHistoryBackgroundLog\(/);
  assert.match(panel,/window\.setInterval\(/);
  assert.match(panel,/window\.clearInterval\(/);
  assert.doesNotMatch(panel,/readHistorySession\s*\(/);
  assert.doesNotMatch(panel,/\.summary\b|\.records\b|\.content\b|\.title\b/);
});
