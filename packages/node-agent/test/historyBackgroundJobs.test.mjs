import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { renderDocument } from '../dist/historyMarkdown.js';
import { beginSessionActivity, observeManagedProcesses } from '../dist/sessionActivity.js';
import {
  managementHistoryBackgroundJobsPayload,
  managementHistoryBackgroundLogPayload,
} from '../dist/managementObservability.js';

function processSession(folderId, id, stdout='compiling…\n') {
  const startedAt = Date.now() - 7_000;
  return {
    id, folderId, startedAt, finalizedAt: undefined, endedAt: undefined,
    postChecksPending: false, verificationOk: true, sensitiveOutput: false,
    stdout, stderr: 'warning: checking…\n',
    stdoutBytes: Buffer.byteLength(stdout), stderrBytes: Buffer.byteLength('warning: checking…\n'),
    stdoutStart: 0, stderrStart: 0, sequence: 1, outputEvents: [], outputEventBytes: 0,
    startupDiagnostics: { attempts: 1, gateWaitMs: 0, retryDelaysMs: [],
      errorDialogSuppressed: false, startupSlots: 4, startIntervalMs: 75 },
    fingerprint: 'test', command: 'cargo test', program: 'cargo', argv: ['test'],
    cwd: '', workspacePath: '', shell:false, interactive:false, stdinOpen:false,
    timedOut:false, killed:false, postChecks:[], operationLockWaitMs:0, resourceLockWaitMs:0,
    detachedGeneration:0, attachmentGeneration:0,
  };
}

test('selected history session lists running tasks and reads redacted bounded logs only from owned folder', async t => {
  const root=await mkdtemp(path.join(tmpdir(),'ctmcp-history-jobs-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const folder=path.join(root,'docs','history-session');
  await mkdir(folder,{recursive:true});
  const sessionKey='opaque-owner-secret';
  await writeFile(path.join(folder,'1.md'),renderDocument(1,'Current session',sessionKey,'unix:1','unix:2','active',[]));
  await writeFile(path.join(folder,'2.md'),renderDocument(2,'Other session','other-owner','unix:1','unix:2','active',[]));
  const owner={};
  const started=beginSessionActivity(owner,sessionKey,'exec_command',{program:'cargo'},Date.now());
  assert.ok(started);
  started.complete('success');
  observeManagedProcesses(owner,sessionKey,'exec_command',{structuredContent:{
    status:'running',session_id:'managed-001'
  }});
  const secret='VISIBLE_SECRET_VALUE_XYZ';
  const process=processSession('repo','managed-001','step 12/24\ntoken='+secret+'\n');
  const other=processSession('other','managed-other','other-user-secret');
  const ctx={
    conversations:owner,
    config:{folders:[{id:'repo',name:'Repository',path:root},{id:'other',name:'Other',path:root+'-other'}]},
    folderRuntimes:new Map([
      ['repo',{sessions:new Map([['managed-001',process]])}],
      ['other',{sessions:new Map([['managed-other',other]])}]
    ])
  };
  const listed=await managementHistoryBackgroundJobsPayload(ctx,1,'repo');
  assert.equal(listed.jobs.length,1);
  assert.equal(listed.jobs[0].id,'managed-001');
  assert.equal(listed.jobs[0].status,'running');
  assert.match(listed.jobs[0].action,/exec_command/);
  assert.doesNotMatch(JSON.stringify(listed),/opaque-owner-secret|other-user-secret/);

  const log=await managementHistoryBackgroundLogPayload(ctx,1,'managed-001','repo');
  assert.equal(log.status,'running');
  assert.match(log.stdout,/step 12\/24/);
  assert.doesNotMatch(log.stdout,new RegExp(secret));
  assert.match(log.stderr,/checking/);
  assert.ok(Buffer.byteLength(log.stdout)<=17_000);
  assert.doesNotMatch(JSON.stringify(log),/opaque-owner-secret|other-user-secret/);

  process.stdout = 'compile-step\n'.repeat(6_000);
  process.stdoutBytes = Buffer.byteLength(process.stdout);
  const bounded = await managementHistoryBackgroundLogPayload(ctx, 1, 'managed-001', 'repo');
  assert.ok(Buffer.byteLength(bounded.stdout) <= 16_384);
  assert.equal(bounded.stdoutTruncated, true);

  await assert.rejects(
    () => managementHistoryBackgroundLogPayload(ctx,2,'managed-001','repo'),
    error => error.code==='HISTORY_BACKGROUND_JOB_NOT_FOUND'
  );
  await assert.rejects(
    () => managementHistoryBackgroundLogPayload(ctx,1,'managed-other','repo'),
    error => error.code==='HISTORY_BACKGROUND_JOB_NOT_FOUND'
  );
  await assert.rejects(
    () => managementHistoryBackgroundLogPayload(ctx,1,'../../etc/passwd','repo'),
    error => error.code==='HISTORY_BACKGROUND_JOB_NOT_FOUND'
  );
  process.endedAt=Date.now();
  process.finalizedAt=Date.now();
  const finished=await managementHistoryBackgroundJobsPayload(ctx,1,'repo');
  assert.deepEqual(finished.jobs,[]);
});

test('background panel appears above checkpoints and follows only the selected session', async () => {
  const { readFile } = await import('node:fs/promises');
  const historyViewer = await readFile(new URL('../../../src/lib/components/HistoryViewer.svelte', import.meta.url),'utf8');
  const panel = await readFile(new URL('../../../src/lib/components/HistoryBackgroundJobs.svelte', import.meta.url),'utf8');
  assert.ok(historyViewer.indexOf('<HistoryBackgroundJobs') < historyViewer.indexOf('{#if detail.records.length > 0}'));
  assert.match(historyViewer, /\{#key selectedSession\.key\}/);
  assert.match(panel, /readHistoryBackgroundLog\(workspaceId, sessionNumber, folderId, jobId\)/);
  assert.match(panel, /listHistoryBackgroundJobs\(workspaceId, sessionNumber, folderId\)/);
  assert.match(panel, /window\.setInterval\(/);
  assert.match(panel, /3_000/);
});

test('history jobs endpoint reads only selected numbered archive and denies missing or escaped sessions',async t=>{
  const root=await mkdtemp(path.join(tmpdir(),'ctmcp-history-numbered-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const history=path.join(root,'docs','history-session');
  await mkdir(history,{recursive:true});
  await writeFile(path.join(history,'1.md'),renderDocument(1,'Here','session-key','unix:1','unix:2','active',[]));
  const ctx={conversations:{},config:{folders:[{id:'repo',name:'Repo',path:root}]}};
  assert.deepEqual((await managementHistoryBackgroundJobsPayload(ctx,1,'repo')).jobs,[]);
  await assert.rejects(
    ()=>managementHistoryBackgroundJobsPayload(ctx,23,'repo'),
    error=>error.code==='HISTORY_SESSION_NOT_FOUND'
  );
  await assert.rejects(
    ()=>managementHistoryBackgroundJobsPayload(ctx,1,'nonexistent'),
    error=>Boolean(error)
  );
});
