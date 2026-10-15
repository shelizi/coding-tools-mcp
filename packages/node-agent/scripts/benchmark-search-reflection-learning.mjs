import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';

delete process.env.CTMCP_DATA_DIR;
delete process.env.CTMCP_CONFIG_FILE;
delete process.env.CTMCP_PORT;
process.env.CTMCP_SHARED_STORE_DISABLED = '1';

const [{ createToolContext }, { callTool }] = await Promise.all([
  import('../dist/server.js'),
  import('../dist/tools.js')
]);
const execFileAsync = promisify(execFile);

function progress(message) { process.stderr.write(`[search-benchmark] ${message}\n`); }
function numericList(name, fallback) {
  const prefix = `--${name}=`;
  const raw = process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length);
  if (!raw) return fallback;
  const values = raw.split(',').map(value => Number.parseInt(value, 10)).filter(value => Number.isFinite(value) && value > 0);
  if (!values.length) throw new Error(`--${name} requires a comma-separated positive numeric list.`);
  return [...new Set(values)];
}
function integerArg(name, fallback) {
  const prefix = `--${name}=`;
  const raw = process.argv.find(arg => arg.startsWith(prefix))?.slice(prefix.length);
  if (raw === undefined) return fallback;
  const value = Number.parseInt(raw, 10);
  if (!Number.isFinite(value) || value < 1) throw new Error(`--${name} must be >= 1.`);
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
function config(root, dataDir) {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode: 'trusted', toolProfile: 'advanced',
    management: { enabled: false },
    oauth: { clientId: 'chatgpt', password: 'benchmark-password', tokenSecret: 'benchmark-token-secret-long-enough' },
    folders: [{ id: 'repo', name: 'Benchmark Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 32, maxOutputBytes: 1024 * 1024 }
  };
}
async function git(root, ...args) {
  return execFileAsync('git', args, { cwd: root, windowsHide: true, maxBuffer: 16 * 1024 * 1024 });
}
function benchmarkPath(index) {
  return path.join('files', String(Math.floor(index / 250)).padStart(4, '0'), `file-${String(index).padStart(6, '0')}.txt`);
}
function fileContent(index) {
  return `benchmark-file=${index}\nmarker-common\nmarker-group-${index % 97}\nstate=clean\n`;
}
async function createFiles(root, fileCount) {
  for (let start = 0; start < fileCount; start += 250) {
    const end = Math.min(fileCount, start + 250);
    await mkdir(path.join(root, path.dirname(benchmarkPath(start))), { recursive: true });
    await Promise.all(Array.from({ length: end - start }, (_, offset) => {
      const index = start + offset;
      return writeFile(path.join(root, benchmarkPath(index)), fileContent(index), 'utf8');
    }));
  }
}
function comparableMatches(value) {
  return (value.matches ?? []).map(match => ({
    path: match.path, line: match.line, column: match.column, end_column: match.end_column,
    query_index: match.query_index, query: match.query, match: match.match
  }));
}
async function flushContext(ctx) {
  await ctx.usageStore.flush();
  await Promise.all([...ctx.folderRuntimes.values()].flatMap(runtime => [runtime.knowledgeIngestor.flush(), runtime.toolEvolutionBenchmarkCollector.flush()]));
}
async function benchmarkRepository(fileCount, iterations) {
  const root = await mkdtemp(path.join(tmpdir(), `ctmcp-search-benchmark-${fileCount}-`));
  const dataDir = await mkdtemp(path.join(tmpdir(), `ctmcp-search-benchmark-state-${fileCount}-`));
  const fixtureStarted = performance.now();
  try {
    await git(root, 'init');
    await git(root, 'config', 'user.email', 'benchmark@example.invalid');
    await git(root, 'config', 'user.name', 'Benchmark');
    progress(`${fileCount} files: create fixture`);
    await createFiles(root, fileCount);
    await git(root, 'add', '.');
    await git(root, 'commit', '-m', 'search benchmark fixture');
    const fixtureCreationMs = performance.now() - fixtureStarted;
    const ctx = await createToolContext(config(root, dataDir));
    const meta = { 'openai/session': `search-benchmark-${fileCount}` };
    const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
    if (selected.ok !== true) throw new Error(`workspace selection failed: ${JSON.stringify(selected)}`);

    const boundedDurations = [];
    const exactDurations = [];
    const boundedScannedFiles = [];
    const exactScannedFiles = [];
    const boundedResponseBytes = [];
    const exactResponseBytes = [];
    const equivalenceFailures = [];
    let exactTotal = null;

    for (let iteration = 0; iteration < iterations; iteration += 1) {
      progress(`${fileCount} files: iteration ${iteration} bounded`);
      const bounded = await timed(() => callTool(ctx, 'search_text', {
        query: 'marker-common', path: '.', max_results: 100, max_preview_bytes: 128, calculate_total: false
      }, meta));
      if (bounded.value.ok !== true) throw new Error(`bounded search failed: ${JSON.stringify(bounded.value)}`);
      progress(`${fileCount} files: iteration ${iteration} exact`);
      const exact = await timed(() => callTool(ctx, 'search_text', {
        query: 'marker-common', path: '.', max_results: 100, max_preview_bytes: 128, calculate_total: true
      }, meta));
      if (exact.value.ok !== true) throw new Error(`exact search failed: ${JSON.stringify(exact.value)}`);

      boundedDurations.push(bounded.elapsedMs);
      exactDurations.push(exact.elapsedMs);
      boundedScannedFiles.push(Number(bounded.value.scanned_files ?? 0));
      exactScannedFiles.push(Number(exact.value.scanned_files ?? 0));
      boundedResponseBytes.push(Buffer.byteLength(JSON.stringify(bounded.value)));
      exactResponseBytes.push(Buffer.byteLength(JSON.stringify(exact.value)));
      exactTotal = Number(exact.value.total_matches ?? 0);
      if (JSON.stringify(comparableMatches(bounded.value)) !== JSON.stringify(comparableMatches(exact.value))) {
        equivalenceFailures.push({ iteration, bounded_count: bounded.value.matches?.length ?? 0, exact_count: exact.value.matches?.length ?? 0 });
      }
      if (exact.value.total_matches_exact !== true || exactTotal !== fileCount) {
        throw new Error(`exact search expected ${fileCount} matches, got ${JSON.stringify({ total_matches: exact.value.total_matches, total_matches_exact: exact.value.total_matches_exact })}`);
      }
    }
    await flushContext(ctx);
    return {
      file_count: fileCount,
      fixture_creation_ms: Number(fixtureCreationMs.toFixed(2)),
      query: 'marker-common',
      max_results: 100,
      bounded_search: { duration: durationStats(boundedDurations), scanned_files: numericStats(boundedScannedFiles), response_bytes: numericStats(boundedResponseBytes) },
      exact_total: { duration: durationStats(exactDurations), scanned_files: numericStats(exactScannedFiles), response_bytes: numericStats(exactResponseBytes), total_matches: exactTotal },
      output_equivalence: { equivalent: equivalenceFailures.length === 0, failures: equivalenceFailures }
    };
  } finally {
    await rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}

const sizes = numericList('sizes', [1_000, 10_000, 50_000]);
const iterations = integerArg('iterations', 5);
const outputPath = stringArg('output');
const result = { schema_version: 1, benchmark: 'reflection-retrieval-learning', section: 'search', generated_at: new Date().toISOString(), iterations, sizes, repositories: [] };
progress(`search run sizes=${sizes.join(',')} iterations=${iterations}`);
for (const size of sizes) {
  result.repositories.push(await benchmarkRepository(size, iterations));
  await persistResult(outputPath, result);
}
process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
