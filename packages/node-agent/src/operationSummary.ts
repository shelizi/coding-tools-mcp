import type { JsonObject } from './types.js';

const OPERATION_RESULT_BOOLEAN_FIELDS = [
  'transport_ok', 'execution_ok', 'command_ok', 'verification_ok',
  'process_timed_out', 'request_timed_out', 'recoverable', 'truncated',
  'stdout_truncated', 'stderr_truncated', 'cursor_expired', 'post_checks_pending',
  'detached', 'deduplicated'
] as const;

const OPERATION_RESULT_TOKEN_FIELDS = [
  'status', 'termination_reason', 'execution_lane', 'outcome_class'
] as const;

const OPERATION_RESULT_INTEGER_FIELDS = [
  'exit_code', 'process_exit_code', 'elapsed_ms', 'actual_wait_ms', 'first_output_ms',
  'stdout_bytes', 'stderr_bytes', 'blocking_queue_wait_ms', 'workspace_admission_wait_ms',
  'global_admission_wait_ms', 'admission_queue_wait_ms', 'workspace_lock_wait_ms',
  'operation_lock_wait_ms', 'resource_lock_wait_ms', 'history_lock_wait_ms',
  'session_registry_wait_ms'
] as const;

function operationSummaryToken(value: unknown): string | undefined {
  return typeof value === 'string' && value.length <= 128 && /^[A-Za-z0-9._:-]+$/.test(value)
    ? value
    : undefined;
}

function operationSummaryInteger(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isSafeInteger(value) ? value : undefined;
}

export function operationResultSummary(name: string, result: JsonObject): JsonObject {
  const summary: JsonObject = {
    ok: result.ok === true,
    tool: name,
    affected_files: result.affected_files ?? null
  };
  for (const field of OPERATION_RESULT_BOOLEAN_FIELDS) {
    if (typeof result[field] === 'boolean') summary[field] = result[field];
  }
  for (const field of OPERATION_RESULT_TOKEN_FIELDS) {
    const value = operationSummaryToken(result[field]);
    if (value !== undefined) summary[field] = value;
  }
  for (const field of OPERATION_RESULT_INTEGER_FIELDS) {
    const value = operationSummaryInteger(result[field]);
    if (value !== undefined) summary[field] = value;
  }
  const error = result.error && typeof result.error === 'object' && !Array.isArray(result.error)
    ? result.error as JsonObject
    : {};
  const errorCode = operationSummaryToken(error.code ?? result.error_code);
  const errorCategory = operationSummaryToken(error.category ?? result.error_category);
  if (errorCode !== undefined) summary.error_code = errorCode;
  if (errorCategory !== undefined) summary.error_category = errorCategory;
  const retryable = error.retryable ?? result.retryable;
  if (typeof retryable === 'boolean') summary.retryable = retryable;
  if (Array.isArray(result.warnings)) summary.warning_count = result.warnings.length;
  return summary;
}
