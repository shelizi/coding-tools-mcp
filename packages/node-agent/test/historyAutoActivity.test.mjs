import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

import { renderDocument, parseCheckpointRecords } from '../dist/historyMarkdown.js';
import {
  AUTO_ACTIVITY_HEADING, MAX_AUTO_ACTIVITY_LINES, mergeAutoActivity, preserveAutoActivity,
} from '../dist/historyAutoMarkdown.js';
import { persistAutoActivity } from '../dist/historyAutoPersist.js';
import { trackMcpToolActivity } from '../dist/sessionActivity.js';
import { acquireHistoryLock, writeHistoryIndex, writeHistoryMarkdown } from '../dist/historyStorage.js';

const event = (id, time=1_800_000_000_000, overrides={}) => ({
  id, atMs:time, tool:'exec_command', action:'exec_command · cargo', status:'success', ...overrides,
});

test('generated progress survives semantic checkpoint rerender, redacts sensitive paths and remains idempotent', () => {
  const before=renderDocument(1,'Autoprogress','owner','unix:1','unix:1','active',[]);
  const captured=mergeAutoActivity(before,[
    event('node:1:100:1:running',100,{status:'running'}),
    event('node:1:100:1:success',200),
    event('node:1:300:3:success',300,{action:'read_file · .ssh/id_rsa'}),
  ]);
  assert.match(captured,/## 自動擷取的工具活動/);
  assert.match(captured,/請求完成/);
  assert.match(captured,/呼叫中/);
  assert.match(captured,/\[sensitive path\]/);
  assert.doesNotMatch(captured,/id_rsa/);
  assert.equal(mergeAutoActivity(captured,[event('node:1:100:1:success',200)]),captured);
  const updated=preserveAutoActivity(renderDocument(1,'Autoprogress','owner','unix:1','unix:2','active',[
    {turn_id:'turn-1',timestamp:'unix:2',user_intent:'completed one step',findings:[],
      decisions:[],files_changed:[],tests:[],runtime_state:[],remaining_issues:[],
      next_actions:[],notes:''}
  ]),captured);
  assert.equal((updated.match(/## 自動擷取的工具活動/g)??[]).length,1);
  assert.equal(parseCheckpointRecords(updated).length,1);
  assert.match(updated,/completed one step/);
  assert.match(updated,/node:1:100:1:success/);
});

test('automatic Markdown progress remains bounded even with hundreds of tool events', () => {
  const before=renderDocument(2,'Capped','owner','unix:1','unix:1','active',[]);
  const events=Array.from({length:180},(_,i)=>event(`rust:123:${i}:success`,i*200));
  const result=mergeAutoActivity(before,events);
  assert.ok((result.match(/<!-- ctmcp-activity:rust:/g)??[]).length<=MAX_AUTO_ACTIVITY_LINES);
  assert.doesNotMatch(result,/rust:123:0:success/);
  assert.match(result,/rust:123:179:success/);
  assert.equal(mergeAutoActivity(result,events.slice(-48)),result);
  assert.ok(result.length<24_000);
  assert.equal(parseCheckpointRecords(result).length,0);
});

test('writer verifies bootstrap mapping and uses cross-runtime exclusive history lock', async t => {
  const root=await mkdtemp(path.join(tmpdir(),'ctmcp-md-progress-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const dir=path.join(root,'docs','history-session');
  await mkdir(dir,{recursive:true});
  const file=path.join(dir,'1.md');
  const previous=renderDocument(1,'Writer','archive-owner','unix:1','unix:1','active',[]);
  await writeHistoryMarkdown(file,previous);
  await writeHistoryIndex(dir,{version:1,latest_number:1,sessions:{
    'archive-owner':{number:1,path:'docs/history-session/1.md',created_at:'unix:1',updated_at:'unix:1'}
  }});
  const ctx={config:{folders:[{id:'repo',name:'Repo',path:root}]}};
  assert.equal(await persistAutoActivity(ctx,'repo','wrong-key',[event('wrong:1')]),false);
  assert.equal(await persistAutoActivity(ctx,'wrong-folder','archive-owner',[event('wrong:2')]),false);
  assert.equal(await readFile(file,'utf8'),previous);

  const lock=await acquireHistoryLock(dir);
  let finished=false;
  const pending=persistAutoActivity(ctx,'repo','archive-owner',[event('node:1:1:success')])
    .then(result=>{finished=true;return result});
  await new Promise(resolve=>setTimeout(resolve,70));
  assert.equal(finished,false);
  assert.equal(await readFile(file,'utf8'),previous);
  await lock.release();
  assert.equal(await pending,true);
  const content=await readFile(file,'utf8');
  assert.match(content,/node:1:1:success/);
  assert.equal(parseCheckpointRecords(content).length,0);

  // Exactly as real checkpoints do: read under lock, regenerate and reattach.
  const lock2=await acquireHistoryLock(dir);
  try {
    const reread=await readFile(file,'utf8');
    const updated=preserveAutoActivity(renderDocument(1,'Writer','archive-owner','unix:1','unix:2','active',[]),reread);
    await writeHistoryMarkdown(file,updated);
  } finally { await lock2.release() }
  assert.match(await readFile(file,'utf8'),/node:1:1:success/);
});

test('writer refuses cross-folder symlink escape and never fabricates a new archive', async t => {
  const root=await mkdtemp(path.join(tmpdir(),'ctmcp-md-no-create-'));
  t.after(()=>rm(root,{recursive:true,force:true}));
  const ctx={config:{folders:[{id:'repo',name:'Repo',path:root}]}};
  assert.equal(await persistAutoActivity(ctx,'repo','absent',[event('node:none')]),false);
  await assert.rejects(readFile(path.join(root,'docs','history-session','1.md'),'utf8'),{code:'ENOENT'});
});

test('Node activity writer preserves Rust-generated lines and shared activity markers', () => {
  const source=renderDocument(1,'Cross-runtime','owner','unix:1','unix:1','active',[]);
  const rustRow='- unix:1800000000.000 · 請求完成 · exec_command <!-- ctmcp-activity:rust:9:123:success -->';
  const section='## 自動擷取的工具活動（非工作結論）\n\n'
    +'<!-- coding-tools-mcp:auto-activity:start -->\n'+rustRow+'\n'
    +'<!-- coding-tools-mcp:auto-activity:end -->\n\n';
  const document=source.replace('## 本轮检查点',section+'## 本轮检查点');
  const merged=mergeAutoActivity(document,[event('node:9:999:success')]);
  assert.match(merged,/rust:9:123:success/);
  assert.match(merged,/node:9:999:success/);
  assert.equal((merged.match(/## 自動擷取的工具活動/g)??[]).length,1);
});

test('a failing persistence callback must never alter a real MCP tool outcome', async () => {
  const owner={};
  const notify=()=>{throw new Error('simulated archive disk error')};
  const result=await trackMcpToolActivity(owner,'session','read_file',{},
    async()=>({ok:true,value:'tool is healthy'}),notify);
  assert.equal(result.value,'tool is healthy');
  await assert.rejects(()=>trackMcpToolActivity(owner,'session','read_file',{},
    async()=>{throw new Error('original tool failure')},notify),/original tool failure/);
});

test('malformed generated block fails closed instead of deleting checkpoint content', () => {
  const source=renderDocument(2,'Safety','owner','unix:1','unix:1','active',[]);
  const broken=source.replace('## 本轮检查点','## 自動擷取的工具活動（非工作結論）\n\n<!-- coding-tools-mcp:auto-activity:start -->\n- malformed\n## 本轮检查点');
  assert.throws(()=>mergeAutoActivity(broken,[event('node:new')]),/Unterminated|Invalid/);
  assert.throws(()=>preserveAutoActivity(source,broken),/Unterminated|Invalid/);
});
