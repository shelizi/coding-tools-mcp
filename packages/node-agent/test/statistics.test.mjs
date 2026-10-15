import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ToolUsageStore, TOOL_USAGE_LOG_FILE, DIAGNOSTICS_LOG_FILE } from '../dist/toolUsage.js';
import { managementTelemetryPayload } from '../dist/managementObservability.js';

const until = Date.UTC(2026, 0, 31, 12, 34, 30);
const intervals = { minute: [60_000, 60], hour: [3_600_000, 24], day: [86_400_000, 30] };
const record = (started, overrides = {}) => ({
  event: 'tool_call', tool: 'read_file', runtime_boot_id: 'old-runtime', server_version: 'old-version',
  started_ts_ms: started, duration_ms: 2, outcome: 'success', ...overrides
});

async function fixture(t, records = [], rotated = [], diagnostics = []) {
  const dir = await mkdtemp(path.join(tmpdir(), 'ctmcp-statistics-'));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const store = new ToolUsageStore(dir, { profileId: 'stats', runtimeBootId: 'new-runtime' });
  await mkdir(store.logDir, { recursive: true });
  const lines = rows => rows.map(row => JSON.stringify(row) + '\n').join('');
  await writeFile(path.join(store.logDir, TOOL_USAGE_LOG_FILE), lines(records));
  await writeFile(path.join(store.logDir, TOOL_USAGE_LOG_FILE + '.1'), lines(rotated));
  await writeFile(path.join(store.logDir, DIAGNOSTICS_LOG_FILE), lines(diagnostics));
  return store;
}

for (const [period, [interval, count]] of Object.entries(intervals)) {
  test(`${period} statistics are bounded, zero-filled and exclude outside/invalid timestamps`, async t => {
    const start = Math.floor((until - 1) / interval) * interval - (count - 1) * interval;
    const store = await fixture(t, [record(start), record(start + interval, { is_error: true }), record(until - 1),
      record(start - 1), record(until), record(-1), record(null), record('invalid')]);
    const result = await store.query({ scope: 'all', statistics_period: period, statistics_until_ts_ms: until });
    assert.equal(result.matched_lines, 3);
    assert.equal(result.statistics.start_ts_ms, start);
    assert.equal(result.statistics.until_ts_ms, until);
    assert.equal(result.statistics.interval_ms, interval);
    assert.equal(result.statistics.buckets.length, count);
    assert.deepEqual(result.statistics.buckets[0], { started_ts_ms: start, calls: 1, errors: 0 });
    assert.equal(result.statistics.buckets[1].errors, 1);
    assert.equal(result.statistics.buckets[2].calls, 0);
    assert.equal(result.statistics.buckets.at(-1).calls, 1);
    assert.equal(result.statistics.buckets.reduce((sum, bucket) => sum + bucket.calls, 0), result.aggregate.calls);
  });
}

test('statistics scan retained logs independently of 200 records and preserve diagnostic deduplication', async t => {
  const calls = Array.from({ length: 250 }, (_, i) => record(until - 10_000, { diagnostic_event_id: `call-${i}` }));
  const store = await fixture(t, [...calls, record(until - 10_000, { event: 'async_session_finalized' }),
    record(until - 10_000, { event: 'transport_event' })], [record(until - 70_000)], calls);
  const result = await store.query({ scope: 'all', limit: 200, include_records: true,
    statistics_period: 'minute', statistics_until_ts_ms: until });
  assert.equal(result.aggregate.calls, 251);
  assert.equal(result.records.length, 200);
  assert.equal(result.statistics.buckets.reduce((sum, bucket) => sum + bucket.calls, 0), 251);
  assert.equal(result.diagnostics_contract.migration.duplicate_records_ignored, 250);
});

test('empty statistics return zero buckets while old queries remain unchanged', async t => {
  const store = await fixture(t);
  const result = await store.query({ scope: 'all', statistics_period: 'day', statistics_until_ts_ms: until });
  assert.equal(result.statistics.buckets.length, 30);
  assert.ok(result.statistics.buckets.every(bucket => bucket.calls === 0 && bucket.errors === 0));
  const legacy = await store.query({ scope: 'all' });
  assert.equal(legacy.statistics, undefined);
});

test('invalid statistics options fail rather than silently becoming an empty chart', async t => {
  const store = await fixture(t);
  for (const args of [{ statistics_period: 'week' }, { statistics_period: 'minute', statistics_until_ts_ms: -1 },
    { statistics_period: 'minute', statistics_until_ts_ms: '100' }]) {
    await assert.rejects(store.query(args), /statistics/i);
  }
});

test('management forwards all-runtime statistics and keeps payloads omitted', async t => {
  const store = await fixture(t, [record(until - 1, { arguments: { secret: 'must-not-appear' } })]);
  const payload = await managementTelemetryPayload({ usageStore: store }, new URLSearchParams({
    statisticsPeriod: 'minute', statisticsUntilTsMs: String(until)
  }));
  assert.equal(payload.scope, 'all');
  assert.equal(payload.statistics.buckets.at(-1).calls, 1);
  assert.equal(payload.records.length, 1);
  assert.ok(!JSON.stringify(payload).includes('must-not-appear'));
  await assert.rejects(managementTelemetryPayload({ usageStore: store }, new URLSearchParams({ statisticsPeriod: 'week' })),
    error => error.status === 400 && error.code === 'INVALID_QUERY');
});
