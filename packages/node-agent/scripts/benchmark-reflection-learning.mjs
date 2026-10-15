import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';

// Benchmarks must never inherit or mutate live Agent state. These values must
// be applied before loading server modules because shared-store capability is
// decided during module initialization.
delete process.env.CTMCP_DATA_DIR;
delete process.env.CTMCP_CONFIG_FILE;
delete process.env.CTMCP_PORT;
process.env.CTMCP_SHARED_STORE_DISABLED = '1';

const [{ createToolContext }, { callTool }] = await Promise.all([
  import('../dist/server.js'),
  import('../dist/tools.js')
]);
const execFileAsync = promisify(execFile);
function progress(message) {
  process.stderr.write(`[benchmark] ${message}\n`);
}

function numericList(name, fallback) {
  const prefix = `--${name}=`;
  const raw = process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length);
  if (!raw) return fallback;
  const values = raw.split(',').map(value => Number.parseInt(value, 10)).filter(value => Number.isFinite(value) && value >= 0);
  if (!values.length) throw new Error(`--${name} requires a comma-separated numeric list.`);
  return [...new Set(values)];
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
  const raw = process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length)?.trim();
  return raw || undefined;
}

async function persistResult(outputPath, value) {
  if (!outputPath) return;
  const resolved = path.resolve(outputPath);
  await mkdir(path.dirname(resolved), { recursive: true });
  const temp = `${resolved}.${process.pid}.tmp`;
  await writeFile(temp, `${JSON.stringify(value, null, 2)}\n`, 'utf8');
  await rename(temp, resolved);
}

function percentile(values, fraction) {
  if (!values.length) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.max(0, Math.ceil(sorted.length * fraction) - 1)];
}

function stats(values) {
  if (!values.length) return { samples: 0, avg_ms: null, p50_ms: null, p95_ms: null, min_ms: null, max_ms: null };
  const total = values.reduce((sum, value) => sum + value, 0);
  return {
    samples: values.length,
    avg_ms: Number((total / values.length).toFixed(2)),
    p50_ms: Number(percentile(values, 0.5).toFixed(2)),
    p95_ms: Number(percentile(values, 0.95).toFixed(2)),
    min_ms: Number(Math.min(...values).toFixed(2)),
    max_ms: Number(Math.max(...values).toFixed(2))
  };
}

function responseBytes(value) {
  return Buffer.byteLength(JSON.stringify(value), 'utf8');
}

async function timed(operation) {
  const started = performance.now();
  const value = await operation();
  return { value, elapsedMs: performance.now() - started };
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
      password: 'benchmark-password',
      tokenSecret: 'benchmark-token-secret-long-enough'
    },
    folders: [{ id: 'repo', name: 'Benchmark Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 }
  };
}

async function git(root, ...args) {
  return execFileAsync('git', args, { cwd: root, windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
}

function benchmarkPath(index) {
  const directory = String(Math.floor(index / 250)).padStart(4, '0');
  return path.join('files', directory, `file-${String(index).padStart(6, '0')}.txt`);
}

function fileContent(index, changed = false) {
  return `benchmark-file=${index}\nmarker-common\nmarker-group-${index % 97}\nstate=${changed ? 'changed' : 'clean'}\n`;
}

async function createFiles(root, fileCount) {
  const batchSize = 250;
  for (let start = 0; start < fileCount; start += batchSize) {
    const end = Math.min(fileCount, start + batchSize);
    const directory = path.dirname(benchmarkPath(start));
    await mkdir(path.join(root, directory), { recursive: true });
    await Promise.all(Array.from({ length: end - start }, (_, offset) => {
      const index = start + offset;
      return writeFile(path.join(root, benchmarkPath(index)), fileContent(index), 'utf8');
    }));
  }
}

async function setChangedFiles(root, fileCount, changedCount) {
  await git(root, 'reset', '--hard', 'HEAD');
  const count = Math.min(fileCount, changedCount);
  const batchSize = 250;
  for (let start = 0; start < count; start += batchSize) {
    const end = Math.min(count, start + batchSize);
    await Promise.all(Array.from({ length: end - start }, (_, offset) => {
      const index = start + offset;
      return writeFile(path.join(root, benchmarkPath(index)), fileContent(index, true), 'utf8');
    }));
  }
  return count;
}

function phaseBaselineMs(value) {
  const parsed = Number(value?.phase_durations_ms?.baseline_capture_ms);
  return Number.isFinite(parsed) ? parsed : null;
}

function metricSummary(samples) {
  return {
    duration: stats(samples.map(sample => sample.duration_ms)),
    baseline_capture: stats(samples.map(sample => sample.baseline_capture_ms).filter(Number.isFinite)),
    response_bytes: {
      samples: samples.length,
      avg: samples.length ? Math.round(samples.reduce((sum, sample) => sum + sample.response_bytes, 0) / samples.length) : null,
      p50: percentile(samples.map(sample => sample.response_bytes), 0.5),
      p95: percentile(samples.map(sample => sample.response_bytes), 0.95),
      max: samples.length ? Math.max(...samples.map(sample => sample.response_bytes)) : null
    }
  };
}

async function flushContext(ctx) {
  await ctx.usageStore.flush();
  await Promise.all([...ctx.folderRuntimes.values()].flatMap(runtime => [
    runtime.knowledgeIngestor.flush(),
    runtime.toolEvolutionBenchmarkCollector.flush()
  ]));
}

async function benchmarkTaskScenario(ctx, meta, fileCount, changedCount, iterations) {
  const startSamples = [];
  const summarySamples = [];
  const finishSamples = [];
  for (let iteration = 0; iteration < iterations; iteration += 1) {
    progress(`${fileCount} files/${changedCount} changed: iteration ${iteration} start_task begin`);
    const start = await timed(() => callTool(ctx, 'start_task', {
      objective: `benchmark task ${fileCount} files ${changedCount} changed iteration ${iteration}`
    }, meta));
    if (start.value.ok !== true) throw new Error(`start_task failed: ${JSON.stringify(start.value)}`);
    progress(`${fileCount} files/${changedCount} changed: iteration ${iteration} start_task done`);
    startSamples.push({
      duration_ms: start.elapsedMs,
      baseline_capture_ms: phaseBaselineMs(start.value),
      response_bytes: responseBytes(start.value)
    });

    const taskId = start.value.task.id;
    progress(`${fileCount} files/${changedCount} changed: iteration ${iteration} change_summary begin`);
    const summary = await timed(() => callTool(ctx, 'change_summary', { task_id: taskId }, meta));
    if (summary.value.ok !== true) throw new Error(`change_summary failed: ${JSON.stringify(summary.value)}`);
    progress(`${fileCount} files/${changedCount} changed: iteration ${iteration} change_summary done`);
    summarySamples.push({
      duration_ms: summary.elapsedMs,
      baseline_capture_ms: phaseBaselineMs(summary.value),
      response_bytes: responseBytes(summary.value)
    });

    progress(`${fileCount} files/${changedCount} changed: iteration ${iteration} finish_task begin`);
    const finish = await timed(() => callTool(ctx, 'finish_task', {
      task_id: taskId,
      summary: 'benchmark completion',
      allow_unverified: true
    }, meta));
    if (finish.value.ok !== true) throw new Error(`finish_task failed: ${JSON.stringify(finish.value)}`);
    progress(`${fileCount} files/${changedCount} changed: iteration ${iteration} finish_task done`);
    finishSamples.push({
      duration_ms: finish.elapsedMs,
      baseline_capture_ms: phaseBaselineMs(finish.value),
      response_bytes: responseBytes(finish.value)
    });
  }
  return {
    start_task: metricSummary(startSamples),
    change_summary: metricSummary(summarySamples),
    finish_task: metricSummary(finishSamples)
  };
}

async function benchmarkRepository(fileCount, changedCounts, iterations) {
  const root = await mkdtemp(path.join(tmpdir(), `ctmcp-benchmark-${fileCount}-`));
  const dataDir = await mkdtemp(path.join(tmpdir(), `ctmcp-benchmark-state-${fileCount}-`));
  const fixtureStarted = performance.now();
  try {
    await git(root, 'init');
    progress(`${fileCount} files: initialize fixture`);
    await git(root, 'config', 'user.email', 'benchmark@example.invalid');
    await git(root, 'config', 'user.name', 'Benchmark');
    await createFiles(root, fileCount);
    await git(root, 'add', '.');
    await git(root, 'commit', '-m', 'benchmark fixture');
    progress(`${fileCount} files: fixture committed`);
    const fixtureCreationMs = performance.now() - fixtureStarted;

    const ctx = await createToolContext(config(root, dataDir));
    progress(`${fileCount} files: tool context ready`);
    const meta = { 'openai/session': `benchmark-${fileCount}` };
    const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
    if (selected.ok !== true) throw new Error(`workspace selection failed: ${JSON.stringify(selected)}`);

    const scenarios = [];
    for (const requestedChangedCount of changedCounts) {
      progress(`${fileCount} files: scenario ${requestedChangedCount} changed`);
      const actualChangedCount = await setChangedFiles(root, fileCount, requestedChangedCount);
      progress(`${fileCount} files: scenario ${requestedChangedCount} dirty state ready`);
      const task = await benchmarkTaskScenario(ctx, meta, fileCount, actualChangedCount, iterations);
      scenarios.push({ requested_changed_files: requestedChangedCount, changed_files: actualChangedCount, task });
    }
    progress(`${fileCount} files: flushing learning writers`);
    await flushContext(ctx);
    progress(`${fileCount} files: complete`);
    return { file_count: fileCount, fixture_creation_ms: Number(fixtureCreationMs.toFixed(2)), scenarios };
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

const sizes = numericList('sizes', [1_000, 10_000, 50_000]);
const changes = numericList('changes', [0, 10, 100, 1_000]);
const iterations = integerArg('iterations', 5);
const outputPath = stringArg('output');
const result = {
  schema_version: 1,
  benchmark: 'reflection-retrieval-learning',
  section: 'task-baseline',
  generated_at: new Date().toISOString(),
  iterations,
  sizes,
  changes,
  repositories: []
};

progress(`task baseline run sizes=${sizes.join(',')} changes=${changes.join(',')} iterations=${iterations}`);
for (const size of sizes) {
  result.repositories.push(await benchmarkRepository(size, changes, iterations));
  await persistResult(outputPath, result);
}
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
