import { getBackend } from "$lib/backend";

export interface TelemetryRecord {
  event?: string;
  started_ts_ms?: number;
  completed_ts_ms?: number;
  duration_ms?: number;
  tool?: string;
  tool_family?: string;
  outcome?: string;
  outcome_class?: string;
  error_code?: string;
  error_category?: string;
  command_kind?: string;
  command_preview?: string;
  request_id?: unknown;
  [key: string]: unknown;
}

export interface TelemetryToolStat {
  tool: string;
  calls: number;
  errors: number;
  warnings: number;
  duration_ms: number;
  avg_ms: number;
  p95_ms: number;
  max_ms: number;
  response_bytes: number;
}

export interface TelemetryAggregate {
  calls: number;
  errors: number;
  warnings: number;
  duration_ms: number;
  avg_ms: number;
  p50_ms: number;
  p95_ms: number;
  max_ms: number;
  response_bytes: number;
  outcomes: Record<string, number>;
  errors_by_code: Record<string, number>;
  tools: TelemetryToolStat[];
}

export interface TelemetryResult {
  workspace_id: string;
  log_dir: string;
  scanned_lines: number;
  matched_lines: number;
  matched_async_session_events: number;
  invalid_complete_lines: number;
  records: TelemetryRecord[];
  aggregate: TelemetryAggregate | null;
  performance: Record<string, unknown> | null;
  warnings: string[];
}

export async function readWorkspaceTelemetry(
  workspaceId: string,
  options: { limit?: number; errorsOnly?: boolean; minDurationMs?: number; sinceTsMs?: number } = {},
): Promise<TelemetryResult> {
  return getBackend().telemetry.query(workspaceId, options);
}
