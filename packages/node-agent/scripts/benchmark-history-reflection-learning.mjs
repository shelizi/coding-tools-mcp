import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';

delete process.env.CTMCP_DATA_DIR;
delete process.env.CTMCP_CONFIG_FILE;
delete process.env.CTMCP_PORT;
process.env.CTMCP_SHARED_STORE_DISABLED = '1';

const [{ createToolContext }, { callTool }] = await Promise.all([
  import('../dist/server.js'),
  import('../dist/tools.js')
]);

function progress(message) {
  process.stderr.write(`[history-benchmark] ${message}\n`);
}

function integerArg(name, fallback, minimum = 1) {
  const prefix = `--${name}=`;
  const raw = process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length);
  if (raw === undefined) return fallback;
  const value = Number.parseInt(raw, 10);
  if (!Number.isFinite(value) || value < minimum) throw new Error(`--${name} must be >= ${minimum}.`);
  return value;
}

function stringArg(name) {
  const prefix = `--${name}=`;
  return process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length)?.trim() || undefined;
}

function percentile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(sorted.length * fraction) - 1)];
}

function durationStats(values) {
  if (!values.length) return { samples: 0, avg_ms: null, p50_ms: null, p95_ms: null, min_ms: null, max_ms: null };
  return {
    samples: values.length,
    avg_ms: Number((values.reduce((sum, value) => sum + value, 0) / values.length).toFixed(2)),
    p50_ms: Number(percentile(values, 0.5).toFixed(2)),
    p95_ms: Number(percentile(values, 0.95).toFixed(2)),
    min_ms: Number(Math.min(...values).toFixed(2)),
    max_ms: Number(Math.max(...values).toFixed(2))
  };
}

function numericStats(values) {
  if (!values.length) return { samples: 0, avg: null, p50: null, p95: null, min: null, max: null };
  return {
    samples: values.length,
    avg: Number((values.reduce((sum, value) => sum + value, 0) / values.length).toFixed(2)),
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    min: Math.min(...values),
    max: Math.max(...values)
  };
}

async function timed(operation) {
  const started = performance.now();
  const value = await operation();
  return { value, elapsedMs: performance.now() - started };
}

async function persistResult(outputPath, value) {
  if (!outputPath) return;
  const resolved = path.resolve(outputPath);
  await mkdir(path.dirname(resolved), { recursive: true });
  const temp = `${resolved}.${process.pid}.tmp`;
  await writeFile(temp, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
  await rename(temp, resolved);
}

function responseBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), 'utf8');
}

function config(root, dataDir) {
  return {
    host: '127.0.0.1',
    port: 0,
    dataDir,
    permissionMode: 'trusted',
    toolProfile: 'advanced',
    management: { enabled: false },
    oauth: {
      clientId: 'chatgpt',
      password: 'history-benchmark-password',
      tokenSecret: 'history-benchmark-token-secret-long-enough'
    },
    folders: [{ id: 'repo', name: 'History Benchmark Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 }
  };
}

function benchmarkMarker(index) {
  const suffix = String(index).padStart(2, '0');
  return `legacy benchmark-key-${suffix} actor-liveness-${suffix} durable-sqlite-${suffix}`;
}

function historyFile(number, sessionKey, marker) {
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

function recallFixtures(sessionCount, fixtureCount) {
  const availableOldSessions = Math.max(1, Math.min(sessionCount - 5, 100));
  const count = Math.min(fixtureCount, availableOldSessions);
  const numbers = [];
  for (let index = 0; index < count; index += 1) {
    const raw = 1 + Math.floor(index * availableOldSessions / count);
    const number = Math.max(1, Math.min(availableOldSessions, raw));
    if (!numbers.includes(number)) numbers.push(number);
  }
  return numbers.map((sessionNumber, index) => ({
    sessionNumber,
    marker: benchmarkMarker(index + 1)
  }));
}

async function writeFixture(root, sessionCount, fixtures) {
  const fixtureBySession = new Map(fixtures.map(fixture => [fixture.sessionNumber, fixture.marker]));
  const historyDir = path.join(root, 'docs', 'history-session');
  await mkdir(historyDir, { recursive: true });
  await writeFile(path.join(historyDir, 'README.md'), '# History archive\n', 'utf8');
  for (let number = 1; number <= sessionCount; number += 1) {
    const marker = fixtureBySession.get(number) ?? `routine settings translation panel ${number}`;
    await writeFile(path.join(historyDir, `${number}.md`), historyFile(number, `history-benchmark-${number}`, marker), 'utf8');
  }
}

async function flushContext(ctx) {
  await ctx.usageStore.flush();
  await Promise.all([...ctx.folderRuntimes.values()].flatMap(runtime => [
    runtime.knowledgeIngestor.flush(),
    runtime.toolEvolutionBenchmarkCollector.flush()
  ]));
}

async function runBenchmark(sessionCount, iterations, fixtureCount, topK) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-history-benchmark-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-history-benchmark-data-'));
  try {
    const fixtures = recallFixtures(sessionCount, fixtureCount);
    const fixtureStarted = performance.now();
    progress(`create ${sessionCount} history sessions with ${fixtures.length} recall fixtures`);
    await writeFixture(root, sessionCount, fixtures);
    const fixtureCreationMs = performance.now() - fixtureStarted;

    const ctx = await createToolContext(config(root, dataDir));
    const meta = { 'openai/session': 'history-benchmark-host-session' };
    const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
    if (selected.ok !== true) throw new Error(`workspace selection failed: ${JSON.stringify(selected)}`);

    progress('repair and build canonical/retrieval indexes');
    const repaired = await timed(() => callTool(ctx, 'history_session_validate', { repair: true }, meta));
    if (repaired.value.ok !== true) throw new Error(`history validation failed: ${JSON.stringify(repaired.value)}`);
    if (repaired.value.retrieval_index_rebuilt !== true) throw new Error('history validation did not rebuild retrieval index');

    const representative = fixtures[0];
    if (!representative) throw new Error('at least one recall fixture is required');

    const searchDurations = [];
    const searchResponseBytes = [];
    for (let iteration = 0; iteration < iterations; iteration += 1) {
      progress(`history_search iteration ${iteration}`);
      const search = await timed(() => callTool(ctx, 'history_search', {
        query: representative.marker,
        top_k: topK,
        include_excerpt: false
      }, meta));
      if (search.value.ok !== true) throw new Error(`history_search failed: ${JSON.stringify(search.value)}`);
      if (search.value.index_source !== 'derived_index') throw new Error(`history_search did not use derived index: ${JSON.stringify(search.value)}`);
      searchDurations.push(search.elapsedMs);
      searchResponseBytes.push(responseBytes(search.value));
    }

    progress(`evaluate top-${topK} recall over ${fixtures.length} planted decisions`);
    const recallDurations = [];
    const recallResults = [];
    for (const fixture of fixtures) {
      const search = await timed(() => callTool(ctx, 'history_search', {
        query: fixture.marker,
        top_k: topK,
        include_excerpt: false
      }, meta));
      if (search.value.ok !== true) throw new Error(`history recall search failed: ${JSON.stringify(search.value)}`);
      const rankIndex = (search.value.results ?? []).findIndex(result => Number(result.session_number) === fixture.sessionNumber);
      recallDurations.push(search.elapsedMs);
      recallResults.push({
        session_number: fixture.sessionNumber,
        query: fixture.marker,
        hit: rankIndex >= 0,
        rank: rankIndex >= 0 ? rankIndex + 1 : null
      });
    }
    const recallHits = recallResults.filter(result => result.hit).length;
    const recallRate = recallResults.length ? recallHits / recallResults.length : 0;

    const bootstrapDurations = [];
    const bootstrapResponseBytes = [];
    let firstBootstrap = null;
    for (let iteration = 0; iteration < iterations; iteration += 1) {
      progress(`history_session_bootstrap iteration ${iteration}`);
      const boot = await timed(() => callTool(ctx, 'history_session_bootstrap', {
        session_key: 'history-benchmark-current',
        recall_query: representative.marker,
        response_mode: 'compact'
      }, meta));
      if (boot.value.ok !== true) throw new Error(`history bootstrap failed: ${JSON.stringify(boot.value)}`);
      if (firstBootstrap === null) firstBootstrap = boot.value;
      if (boot.value.response_mode !== 'compact') throw new Error('history bootstrap did not stay compact');
      if (responseBytes(boot.value) >= 128 * 1024) throw new Error(`history bootstrap exceeded 128 KiB: ${responseBytes(boot.value)}`);
      if (!(boot.value.relevant_history_summaries ?? []).some(summary => Number(summary.number) === representative.sessionNumber)) {
        throw new Error(`history bootstrap failed to recall planted decision: ${JSON.stringify(boot.value.relevant_history_summaries)}`);
      }
      bootstrapDurations.push(boot.elapsedMs);
      bootstrapResponseBytes.push(responseBytes(boot.value));
    }

    await flushContext(ctx);
    return {
      session_count: sessionCount,
      fixture_creation_ms: Number(fixtureCreationMs.toFixed(2)),
      index_rebuild: {
        duration_ms: Number(repaired.elapsedMs.toFixed(2)),
        retrieval_index_rebuilt: repaired.value.retrieval_index_rebuilt === true,
        history_count: repaired.value.history_count ?? sessionCount
      },
      history_search: {
        query: representative.marker,
        top_k: topK,
        duration: durationStats(searchDurations),
        response_bytes: numericStats(searchResponseBytes),
        index_document_count: sessionCount
      },
      top_k_recall: {
        top_k: topK,
        fixture_count: recallResults.length,
        hits: recallHits,
        recall_rate: Number(recallRate.toFixed(4)),
        recall_percent: Number((recallRate * 100).toFixed(2)),
        duration: durationStats(recallDurations),
        misses: recallResults.filter(result => !result.hit),
        ranks: recallResults.map(result => result.rank)
      },
      bootstrap: {
        session_key: 'history-benchmark-current',
        response_mode: 'compact',
        duration: durationStats(bootstrapDurations),
        response_bytes: numericStats(bootstrapResponseBytes),
        first_history_count: firstBootstrap?.history_count ?? null,
        payload_bounded: firstBootstrap?.payload_bounded === true,
        retrieval_source: firstBootstrap?.history_retrieval_source ?? null
      }
    };
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

const sessions = integerArg('sessions', 180);
const iterations = integerArg('iterations', 5);
const fixtures = integerArg('fixtures', 20);
const topK = Math.min(20, integerArg('top-k', 5));
const outputPath = stringArg('output');
const result = {
  schema_version: 1,
  benchmark: 'reflection-retrieval-learning',
  section: 'history',
  generated_at: new Date().toISOString(),
  sessions,
  iterations,
  fixtures,
  top_k: topK,
  result: await runBenchmark(sessions, iterations, fixtures, topK)
};
await persistResult(outputPath, result);
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
