import test from 'node:test';
import assert from 'node:assert/strict';
import {
  access, mkdir, mkdtemp, readFile, rm, utimes, writeFile
} from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';
import { HistoryError } from '../dist/history.js';
import {
  acquireHistoryLock, HISTORY_INDEX_FILE, HISTORY_LOCK_DIR
} from '../dist/historyStorage.js';

function config(root, dataDir) {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode: 'trusted', toolProfile: 'advanced',
    management: { enabled: false },
    oauth: { clientId: 'chatgpt', password: 'history-test-password', tokenSecret: 'history-test-token-secret-long-enough' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 }
  };
}

async function fixture(t, prefix = 'ctmcp-history-') {
  const root = await mkdtemp(path.join(tmpdir(), `${prefix}root-`));
  const dataDir = await mkdtemp(path.join(tmpdir(), `${prefix}data-`));
  t.after(async () => {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const ctx = await createToolContext(config(root, dataDir));
  const meta = { 'openai/session': `${prefix}${Math.random().toString(36).slice(2)}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  return { root, dataDir, ctx, meta };
}

async function pathExists(value) {
  try { await access(value); return true; } catch { return false; }
}

function historyFile(number, sessionKey, marker = `marker-${number}`) {
  return `# 会话 ${number}：${marker}\n\n`
    + `**Session key:** ${sessionKey}\n`
    + '**Created:** 2026-07-17T08:00:00+08:00\n'
    + '**Updated:** 2026-07-17T09:00:00+08:00\n'
    + '**Status:** completed\n\n'
    + `## 用户核心目标\n\n- 目标-${marker}\n\n`
    + `## 已确认事实\n\n- 事实-${marker}\n\n`
    + `## 已完成修改\n\n- 修改-${marker}\n\n`
    + `## 关键设计决定\n\n- 决定-${marker}\n\n`
    + `## 测试结果\n\n- 测试-${marker}\n\n`
    + `## 当前运行状态\n\n- 运行-${marker}\n\n`
    + `## 剩余问题\n\n- 问题-${marker}\n\n`
    + `## 下一步\n\n- 下一步-${marker}\n\n`
    + '## 本轮检查点\n\n';
}

async function writeHistory(root, files) {
  const dir = path.join(root, 'docs', 'history-session');
  await mkdir(dir, { recursive: true });
  await writeFile(path.join(dir, 'README.md'), '# History archive\n');
  for (const [name, content] of Object.entries(files)) await writeFile(path.join(dir, name), content);
  return dir;
}

test('bootstrap writes the shared Rust index format and resumes idempotently', async t => {
  const { root, ctx, meta } = await fixture(t);
  const dir = await writeHistory(root, {
    '1.md': historyFile(1, 'old-session-1', 'first'),
    '2.md': historyFile(2, 'old-session-2', 'second')
  });

  const first = await callTool(ctx, 'history_session_bootstrap', {
    session_key: 'current-chat', title: '继续开发'
  }, meta);
  assert.equal(first.ok, true);
  assert.equal(first.current_number, 3);
  assert.equal(first.current_path, 'docs/history-session/3.md');
  assert.equal(Object.hasOwn(first, 'expected_path'), false);
  assert.equal(first.created, true);
  assert.equal(first.resumed, false);
  assert.deepEqual(first.history_numbers, [1, 2]);
  assert.equal(first.history_loaded_count, 2);
  assert.equal(first.history_read_mode, 'scan_rebuild_recent_summaries_plus_latest_bounded');
  assert.match(first.history_digest, /^[0-9a-f]{64}$/);
  assert.match(first.all_history_summary, /决定-first/);
  assert.match(first.latest_handoff, /second/);

  const index = JSON.parse(await readFile(path.join(dir, HISTORY_INDEX_FILE), 'utf8'));
  assert.equal(index.version, 1);
  assert.equal(index.latest_number, 3);
  assert.deepEqual(Object.keys(index.sessions).sort(), ['current-chat', 'old-session-1', 'old-session-2']);
  assert.deepEqual(index.sessions['current-chat'], {
    number: 3,
    path: 'docs/history-session/3.md',
    created_at: index.sessions['current-chat'].created_at,
    updated_at: index.sessions['current-chat'].updated_at
  });
  assert.equal(await pathExists(path.join(dir, 'node-agent-index.json')), false);

  const second = await callTool(ctx, 'history_session_bootstrap', {
    session_key: 'current-chat', title: '标题变化不会新建'
  }, meta);
  assert.equal(second.ok, true);
  assert.equal(second.current_number, 3);
  assert.equal(second.created, false);
  assert.equal(second.resumed, true);
  assert.equal(await pathExists(path.join(dir, '4.md')), false);
});

test('checkpoint upserts turns, ignores exact duplicates, generates stable IDs and redacts secrets', async t => {
  const { root, ctx, meta } = await fixture(t);
  const boot = await callTool(ctx, 'history_session_bootstrap', { session_key: 'checkpoint-chat' }, meta);
  assert.equal(boot.ok, true);

  const args = {
    session_key: 'checkpoint-chat', expected_path: boot.current_path,
    turn_id: 'turn-0001', timestamp: '2026-07-17T11:00:00+08:00',
    user_intent: '实现归档', findings: ['接口已确认'],
    decisions: ['使用 bearer history-super-secret-token'],
    files_changed: ['src/history.ts'], tests: ['node test passed'],
    runtime_state: ['服务运行中'], remaining_issues: ['无'],
    next_actions: ['继续验证'], notes: 'password=hunter2'
  };
  const first = await callTool(ctx, 'history_session_checkpoint', args, meta);
  assert.equal(first.ok, true);
  assert.equal(first.updated, true);
  assert.equal(first.duplicate_ignored, false);
  assert.match(first.content_hash, /^[0-9a-f]{64}$/);
  assert.ok(first.warnings.length > 0);

  const duplicate = await callTool(ctx, 'history_session_checkpoint', args, meta);
  assert.equal(duplicate.ok, true);
  assert.equal(duplicate.updated, false);
  assert.equal(duplicate.duplicate_ignored, true);

  const updated = await callTool(ctx, 'history_session_checkpoint', {
    ...args, next_actions: ['运行完整回归']
  }, meta);
  assert.equal(updated.ok, true);
  assert.equal(updated.updated, true);
  assert.equal(updated.duplicate_ignored, false);

  const automaticArgs = {
    session_key: 'checkpoint-chat', expected_path: boot.current_path,
    user_intent: '保存当前进度', findings: ['工具目录缓存已确认'],
    next_actions: ['重新配置连接后新开会话']
  };
  const automatic = await callTool(ctx, 'history_session_checkpoint', automaticArgs, meta);
  const automaticDuplicate = await callTool(ctx, 'history_session_checkpoint', automaticArgs, meta);
  assert.match(automatic.turn_id, /^auto-[0-9a-f]{16}$/);
  assert.equal(automaticDuplicate.turn_id, automatic.turn_id);
  assert.equal(automaticDuplicate.duplicate_ignored, true);

  const content = await readFile(path.join(root, boot.current_path), 'utf8');
  assert.equal(content.match(/### turn-0001/g)?.length, 1);
  assert.equal(content.match(new RegExp(`### ${automatic.turn_id}`, 'g'))?.length, 1);
  assert.match(content, /运行完整回归/);
  assert.doesNotMatch(content, /继续验证/);
  assert.match(content, /\[REDACTED\]/);
  assert.doesNotMatch(content, /history-super-secret-token|hunter2/);
});

test('missing or corrupt index rebuilds from Markdown and duplicate session keys are rejected', async t => {
  const { root, ctx, meta } = await fixture(t);
  const dir = await writeHistory(root, {
    '1.md': historyFile(1, 'rust-session', 'rust-compatible')
  });
  await writeFile(path.join(dir, HISTORY_INDEX_FILE), '{broken-json');

  const resumed = await callTool(ctx, 'history_session_bootstrap', {
    session_key: 'rust-session'
  }, meta);
  assert.equal(resumed.ok, true);
  assert.equal(resumed.current_number, 1);
  assert.equal(resumed.created, false);
  assert.equal(resumed.history_read_mode, 'scan_rebuild_recent_summaries_plus_latest_bounded');
  const repairedIndex = JSON.parse(await readFile(path.join(dir, HISTORY_INDEX_FILE), 'utf8'));
  assert.equal(repairedIndex.sessions['rust-session'].number, 1);

  await writeFile(path.join(dir, '2.md'), historyFile(2, 'duplicate-key', 'a'));
  await writeFile(path.join(dir, '3.md'), historyFile(3, 'duplicate-key', 'b'));
  await rm(path.join(dir, HISTORY_INDEX_FILE), { force: true });
  const conflict = await callTool(ctx, 'history_session_bootstrap', { session_key: 'new-session' }, meta);
  assert.equal(conflict.ok, false);
  assert.equal(conflict.error.code, 'HISTORY_INDEX_CONFLICT');
  assert.deepEqual(conflict.error.details.duplicate_session_keys, ['duplicate-key']);
});

test('validate reports gaps, invalid and empty files and repairs only the derived index', async t => {
  const { root, ctx, meta } = await fixture(t);
  const dir = await writeHistory(root, {
    '1.md': historyFile(1, 'gap-one', 'one'),
    '3.md': historyFile(3, 'gap-three', 'three'),
    '4.md': '',
    'bad.md': 'invalid'
  });

  const readonly = await callTool(ctx, 'history_session_validate', { repair: false }, meta);
  assert.equal(readonly.ok, true);
  assert.equal(readonly.sequence_valid, false);
  assert.deepEqual(readonly.numbers, [1, 3, 4]);
  assert.deepEqual(readonly.missing_numbers, [2]);
  assert.ok(readonly.invalid_files.includes('bad.md'));
  assert.ok(readonly.empty_files.includes('4.md'));
  assert.equal(readonly.latest_number, 4);
  assert.equal(readonly.latest_path, 'docs/history-session/4.md');
  assert.equal(await pathExists(path.join(dir, '2.md')), false);

  await writeFile(path.join(dir, HISTORY_INDEX_FILE), '{broken-json');
  const repaired = await callTool(ctx, 'history_session_validate', { repair: true }, meta);
  assert.equal(repaired.ok, true);
  assert.equal(repaired.repaired, true);
  assert.equal(repaired.index_status, 'invalid');
  assert.equal(await pathExists(path.join(dir, '2.md')), false);
  assert.equal(await pathExists(path.join(dir, 'bad.md')), true);
  const index = JSON.parse(await readFile(path.join(dir, HISTORY_INDEX_FILE), 'utf8'));
  assert.equal(index.sessions['gap-one'].number, 1);
  assert.equal(index.sessions['gap-three'].number, 3);
});

test('concurrent Agent contexts allocate distinct history numbers through the shared directory lock', async t => {
  const state = await fixture(t, 'ctmcp-history-parallel-');
  const secondDataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-history-parallel-data-'));
  t.after(() => rm(secondDataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const second = await createToolContext(config(state.root, secondDataDir));
  const secondMeta = { 'openai/session': 'parallel-second' };
  assert.equal((await callTool(second, 'switch_workspace_folder', { folder_id: 'repo' }, secondMeta)).ok, true);

  const [left, right] = await Promise.all([
    callTool(state.ctx, 'history_session_bootstrap', { session_key: 'parallel-a' }, state.meta),
    callTool(second, 'history_session_bootstrap', { session_key: 'parallel-b' }, secondMeta)
  ]);
  assert.equal(left.ok, true);
  assert.equal(right.ok, true);
  assert.deepEqual([left.current_number, right.current_number].sort((a, b) => a - b), [1, 2]);
  assert.equal(await pathExists(path.join(state.root, 'docs/history-session/1.md')), true);
  assert.equal(await pathExists(path.join(state.root, 'docs/history-session/2.md')), true);
  const index = JSON.parse(await readFile(path.join(state.root, 'docs/history-session/index.json'), 'utf8'));
  assert.equal(Object.keys(index.sessions).length, 2);
});

test('directory lock times out under contention and recovers a stale owner', async t => {
  const { root } = await fixture(t, 'ctmcp-history-lock-');
  const dir = path.join(root, 'docs/history-session');
  const first = await acquireHistoryLock(dir, { timeoutMs: 200, retryMs: 5, staleMs: 2_000 });
  await assert.rejects(
    acquireHistoryLock(dir, { timeoutMs: 40, retryMs: 5, staleMs: 2_000 }),
    error => error instanceof HistoryError && error.code === 'HISTORY_LOCK_TIMEOUT' && error.retryable === true
  );
  await first.release();

  const staleDir = path.join(dir, HISTORY_LOCK_DIR);
  await mkdir(staleDir);
  await writeFile(path.join(staleDir, 'owner.json'), JSON.stringify({ version: 1, token: 'abandoned' }));
  const old = new Date(Date.now() - 5_000);
  await utimes(staleDir, old, old);
  const recovered = await acquireHistoryLock(dir, { timeoutMs: 200, retryMs: 5, staleMs: 500 });
  await recovered.release();
  assert.equal(await pathExists(staleDir), false);
});

test('bootstrap bounds summaries and inherited history without recursive growth', async t => {
  const { root, ctx, meta } = await fixture(t);
  const files = {};
  const marker = 'X'.repeat(4_000);
  for (let number = 1; number <= 20; number += 1) {
    files[`${number}.md`] = historyFile(number, `session-${number}`, `${number}-${marker}`);
  }
  const dir = await writeHistory(root, files);

  const boot = await callTool(ctx, 'history_session_bootstrap', { session_key: 'bounded-summary' }, meta);
  assert.equal(boot.ok, true);
  assert.equal(boot.history_count, 20);
  assert.equal(boot.history_loaded_count, 12);
  assert.equal(boot.history_omitted_count, 8);
  assert.equal(boot.payload_bounded, true);
  const content = await readFile(path.join(dir, '21.md'), 'utf8');
  assert.match(content, /个较早会话未展开/);
  assert.ok(Array.from(content).length < 20_000);
  assert.equal(content.match(/## 继承的历史摘要/g)?.length, 1);

  const checkpoint = await callTool(ctx, 'history_session_checkpoint', {
    session_key: boot.session_key, expected_path: boot.current_path,
    turn_id: 'bounded-turn', user_intent: '继续实现'
  }, meta);
  assert.equal(checkpoint.ok, true);
  const updated = await readFile(path.join(dir, '21.md'), 'utf8');
  assert.equal(updated.match(/## 继承的历史摘要/g)?.length, 1);
  assert.match(updated, /继续实现/);
});

test('history paths and stable checkpoint targets cannot escape or cross sessions', async t => {
  const { root, ctx, meta } = await fixture(t);
  const outside = await callTool(ctx, 'history_session_validate', { history_dir: '../outside', repair: false }, meta);
  assert.equal(outside.ok, false);
  assert.equal(outside.error.code, 'PATH_OUTSIDE_WORKSPACE');

  const missing = await callTool(ctx, 'history_session_checkpoint', {
    history_dir: 'missing-history',
    session_key: 'missing-session', expected_path: 'missing-history/1.md', turn_id: 'missing-turn'
  }, meta);
  assert.equal(missing.ok, false);
  assert.equal(missing.error.code, 'SESSION_NOT_BOOTSTRAPPED');
  assert.equal(await pathExists(path.join(root, 'missing-history')), false);

  const first = await callTool(ctx, 'history_session_bootstrap', { session_key: 'session-a' }, meta);
  const second = await callTool(ctx, 'history_session_bootstrap', { session_key: 'session-b' }, meta);
  assert.equal(first.ok, true);
  assert.equal(second.ok, true);
  const wrongTarget = await callTool(ctx, 'history_session_checkpoint', {
    session_key: 'session-a', expected_path: second.current_path, turn_id: 'wrong-target'
  }, meta);
  assert.equal(wrongTarget.ok, false);
  assert.equal(wrongTarget.error.code, 'SESSION_TARGET_MISMATCH');
  assert.equal(await pathExists(path.join(root, 'docs/history-session/3.md')), false);
});
