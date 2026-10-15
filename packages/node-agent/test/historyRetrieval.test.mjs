import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import {
  buildHistoryRetrievalIndex, historyRetrievalTokens, searchHistoryRetrievalIndex
} from '../dist/historyRetrieval.js';
import {
  HISTORY_RETRIEVAL_INDEX_FILE, readHistoryRetrievalIndex, writeHistoryRetrievalIndex
} from '../dist/historyStorage.js';

function document(number, fields = {}) {
  const values = {
    title: fields.title ?? `session-${number}`,
    intent: fields.intent ?? `一般工作 ${number}`,
    findings: fields.findings ?? `一般發現 ${number}`,
    files: fields.files ?? `src/general-${number}.ts`,
    decisions: fields.decisions ?? `一般決策 ${number}`,
    tests: fields.tests ?? `test ${number} passed`,
    runtime: fields.runtime ?? `runtime ${number}`,
    issues: fields.issues ?? `一般問題 ${number}`,
    next: fields.next ?? `一般下一步 ${number}`
  };
  const content = `# 会话 ${number}：${values.title}\n\n`
    + `**Session key:** session-${number}\n`
    + '**Created:** unix:1\n'
    + `**Updated:** unix:${number}\n`
    + '**Status:** completed\n\n'
    + `## 用户核心目标\n\n- ${values.intent}\n\n`
    + `## 已确认事实\n\n- ${values.findings}\n\n`
    + `## 已完成修改\n\n- ${values.files}\n\n`
    + `## 关键设计决定\n\n- ${values.decisions}\n\n`
    + `## 测试结果\n\n- ${values.tests}\n\n`
    + `## 当前运行状态\n\n- ${values.runtime}\n\n`
    + `## 剩余问题\n\n- ${values.issues}\n\n`
    + `## 下一步\n\n- ${values.next}\n\n`
    + '## 本轮检查点\n\n';
  return {
    number,
    path: `docs/history-session/${number}.md`,
    content,
    session_key: `session-${number}`,
    created_at: 'unix:1',
    updated_at: `unix:${number}`
  };
}

test('history retrieval ranks an old relevant session above newer unrelated noise', () => {
  const documents = [
    document(3, {
      title: '終端持久化修復',
      intent: '修正終端機閒置後斷線並保存 durable managed job',
      findings: 'Host actor 被 SQLite 阻塞造成 liveness timeout',
      decisions: 'durable session 以 operation id 重新 attach',
      files: 'packages/node-agent/src/processes/managedJobs.ts'
    }),
    ...Array.from({ length: 12 }, (_, index) => document(20 + index, {
      intent: `更新設定頁與一般 UI ${index}`,
      findings: `近期無關工作 ${index}`,
      decisions: `維持 UI layout ${index}`
    }))
  ];
  const index = buildHistoryRetrievalIndex(documents);
  const [top] = searchHistoryRetrievalIndex(index, '終端持久化 durable session', { topK: 5 });
  assert.equal(top.sessionNumber, 3);
  assert.ok(top.matchedFields.includes('userIntent') || top.matchedFields.includes('title'));
});

test('history retrieval supports Traditional Chinese partial wording with CJK bigrams and trigrams', () => {
  const index = buildHistoryRetrievalIndex([
    document(5, {
      intent: '修正檔案管理員開啟檔案時沒有定位到對應資料夾',
      findings: 'Windows Explorer 需要 select 目標檔案',
      decisions: '資料夾維持直接開啟，檔案使用選取模式'
    }),
    document(6, { intent: '更新模型設定與 API provider' })
  ]);
  const hits = searchHistoryRetrievalIndex(index, '檔案管理開啟定位');
  assert.equal(hits[0].sessionNumber, 5);
  assert.ok(historyRetrievalTokens('檔案管理開啟定位').includes('檔案'));
});

test('history retrieval boosts exact file and identifier matches', () => {
  const index = buildHistoryRetrievalIndex([
    document(8, { files: 'packages/node-agent/src/knowledge/validation.ts', intent: '調整驗證規則' }),
    document(9, { files: 'packages/node-agent/src/history.ts', intent: '更新歷史摘要' })
  ]);
  const [top] = searchHistoryRetrievalIndex(index, 'knowledge/validation.ts');
  assert.equal(top.sessionNumber, 8);
  assert.ok(top.matchedFields.includes('filesChanged'));
});

test('history retrieval index build and persistence are deterministic', async t => {
  const dir = await mkdtemp(path.join(tmpdir(), 'ctmcp-history-retrieval-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const input = [document(2), document(1, { intent: '第一個舊工作' })];
  const first = buildHistoryRetrievalIndex(input);
  const second = buildHistoryRetrievalIndex([...input].reverse());
  assert.deepEqual(second, first);
  assert.deepEqual(first.documents.map(item => item.sessionNumber), [1, 2]);
  assert.match(first.sourceDigest, /^[0-9a-f]{64}$/);

  await writeHistoryRetrievalIndex(dir, first);
  assert.deepEqual(await readHistoryRetrievalIndex(dir), first);
  const bytes = await readFile(path.join(dir, HISTORY_RETRIEVAL_INDEX_FILE), 'utf8');
  assert.equal(bytes, `${JSON.stringify(first, null, 2)}\n`);
});
