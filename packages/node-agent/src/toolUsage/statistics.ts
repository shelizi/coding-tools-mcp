import type { JsonObject } from '../types.js';

const PERIODS = { minute: [60_000, 60], hour: [3_600_000, 24], day: [86_400_000, 30] } as const;
const MAX_TIMESTAMP = 8_640_000_000_000_000;

export class UsageStatistics {
  readonly period: keyof typeof PERIODS;
  readonly intervalMs: number;
  readonly startTsMs: number;
  readonly untilTsMs: number;
  readonly buckets: Array<{ started_ts_ms: number; calls: number; errors: number }>;

  constructor(period: unknown, until: unknown) {
    if (typeof period !== 'string' || !Object.hasOwn(PERIODS, period)) {
      throw new Error('Unsupported statistics period.');
    }
    if (typeof until !== 'number' || !Number.isSafeInteger(until) || until < 1 || until > MAX_TIMESTAMP) {
      throw new Error('Invalid statistics cutoff timestamp.');
    }
    this.period = period as keyof typeof PERIODS;
    const [interval, count] = PERIODS[this.period];
    this.intervalMs = interval;
    this.untilTsMs = until;
    const last = Math.floor((until - 1) / interval) * interval;
    this.startTsMs = Math.max(0, last - (count - 1) * interval);
    const bucketCount = Math.min(count, Math.floor(last / interval) + 1);
    this.buckets = Array.from({ length: bucketCount }, (_, index) => ({
      started_ts_ms: this.startTsMs + index * interval, calls: 0, errors: 0
    }));
  }

  includes(record: JsonObject): boolean {
    const timestamp = record.started_ts_ms;
    return typeof timestamp === 'number' && Number.isSafeInteger(timestamp)
      && timestamp >= this.startTsMs && timestamp < this.untilTsMs;
  }

  accumulate(record: JsonObject, isError: boolean): void {
    const index = Math.floor((record.started_ts_ms as number - this.startTsMs) / this.intervalMs);
    this.buckets[index].calls += 1;
    if (isError) this.buckets[index].errors += 1;
  }

  report(): JsonObject {
    return { period: this.period, interval_ms: this.intervalMs, start_ts_ms: this.startTsMs,
      until_ts_ms: this.untilTsMs, buckets: this.buckets };
  }
}

export function usageStatistics(args: JsonObject): UsageStatistics | null {
  return args.statistics_period == null ? null
    : new UsageStatistics(args.statistics_period, args.statistics_until_ts_ms ?? Date.now());
}
