import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { renderDocument, parseCheckpointRecords } from '../dist/historyMarkdown.js';
import { scheduleAutoActivity } from '../dist/historyAutoPersist.js';
import { writeHistoryIndex, writeHistoryMarkdown } from '../dist/historyStorage.js';
import { beginSessionActivity, observeManagedProcesses } from '../dist/sessionActivity.js';

test('MCP activity and real background start/finish automatically flush to bootstrapped .md', {timeout: 32_000}, async t => {
  const root=await mkdtemp(path.join(tmpdir(),'ctmcp-md-auto-live-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const dir=path.join(root,'docs','history-session');
  await mkdir(dir,{recursive:true});
  await writeHistoryMarkdown(path.join(dir,'1.md'),
    renderDocument(1,'Live','active-conversation','unix:1','unix:1','active',[]));
  await writeHistoryIndex(dir,{version:1,latest_number:1,sessions:{
    'active-conversation':{number:1,path:'docs/history-session/1.md',created_at:'unix:1',updated_at:'unix:1'}
  }});
  const now=Date.now();
  const managed={id:'job1',startedAt:now,endedAt:undefined,finalizedAt:undefined,
    postChecksPending:false,terminationReason:undefined};
  const owner={};
  const ctx={
    conversations:owner,
    config:{folders:[{id:'repo',name:'Repo',path:root}]},
    selections:new Map([['active-conversation','repo']]),
    folderRuntimes:new Map([['repo',{sessions:new Map([['job1',managed]])}]]),
  };
  const guard=beginSessionActivity(owner,'active-conversation','exec_command',{program:'cargo'});
  assert.ok(guard);
  observeManagedProcesses(owner,'active-conversation','exec_command',
    {structuredContent:{status:'running',session_id:'job1'}});
  scheduleAutoActivity(ctx,'active-conversation');
  guard.complete('success');
  scheduleAutoActivity(ctx,'active-conversation');
  await new Promise(resolve=>setTimeout(resolve,5_600));
  let content=await readFile(path.join(dir,'1.md'),'utf8');
  assert.match(content,/背景程序執行中/);
  assert.match(content,/請求完成/);
  assert.match(content,/呼叫中/);
  const count=content.match(/<!-- ctmcp-activity:/g)?.length??0;
  managed.endedAt=Date.now();
  managed.finalizedAt=managed.endedAt;
  await new Promise(resolve=>setTimeout(resolve,10_700));
  content=await readFile(path.join(dir,'1.md'),'utf8');
  assert.match(content,/背景程序已結束/);
  assert.ok((content.match(/<!-- ctmcp-activity:/g)?.length??0)>count);
  assert.equal(parseCheckpointRecords(content).length,0);
});
