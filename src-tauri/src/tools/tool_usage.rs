use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError};

const TOOL_USAGE_LOG_FILE: &str = "mcp-tool-usage.jsonl";
const MAX_ROTATED_FILES: usize = 5;
const DEFAULT_BURST_IDLE_MS: u64 = 120_000;

#[derive(Default)]
struct ToolStats {
    calls: u64,
    errors: u64,
    warnings: u64,
    duration_ms: u128,
    queue_wait_ms: u128,
    workspace_admission_wait_ms: u128,
    global_admission_wait_ms: u128,
    blocking_queue_wait_ms: u128,
    workspace_lock_wait_ms: u128,
    history_lock_wait_ms: u128,
    session_registry_wait_ms: u128,
    actual_wait_ms: u128,
    snapshot_ms: u128,
    resource_lock_wait_ms: u128,
    operation_lock_wait_ms: u128,
    batch_queue_wait_ms: u128,
    queue_nonzero: u64,
    request_bytes: u64,
    response_bytes: u64,
    recovery_actions: u64,
    failed_command_ids: u64,
    skipped_command_ids: u64,
    empty_wait_timeouts: u64,
    deduplicated_calls: u64,
    heartbeat_responses: u64,
    detached_responses: u64,
    format_files_requested: u64,
    format_files_supported: u64,
    format_files_changed: u64,
    format_files_unchanged: u64,
    format_files_skipped: u64,
    formatter_groups: u64,
    custom_formatter_groups: u64,
    unavailable_adapters: u64,
    unexpected_changes: u64,
    format_diff_bytes: u64,
    format_apply_calls: u64,
    durations: Vec<u64>,
}

#[derive(Default)]
struct CommandKindStats {
    calls: u64,
    server_duration_ms: u128,
    child_sessions: u64,
    child_process_ms: u128,
}

#[derive(Default)]
struct BurstStats {
    calls: u64,
    first_started_ts_ms: u64,
    last_completed_ts_ms: u64,
    server_duration_ms: u128,
    orchestration_gap_ms: u128,
    tools: BTreeMap<String, u64>,
}

#[derive(Default)]
struct PerformanceStats {
    tool_calls: u64,
    async_sessions: u64,
    first_started_ts_ms: u64,
    last_completed_ts_ms: u64,
    server_duration_ms: u128,
    queue_wait_ms: u128,
    active_orchestration_gap_ms: u128,
    idle_gap_ms: u128,
    idle_gap_count: u64,
    child_process_ms: u128,
    orchestration_gaps: Vec<u64>,
    child_durations: Vec<u64>,
    first_output_durations: Vec<u64>,
    child_failures: u64,
    child_terminations: BTreeMap<String, u64>,
    command_kinds: BTreeMap<String, CommandKindStats>,
    bursts: BTreeMap<String, BurstStats>,
    inferred_burst_id: u64,
    previous_completed_ts_ms: u64,
}

pub fn query_tool_usage(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    query_tool_usage_for_profile(&ctx.profile_id, args)
}

/// Query telemetry for a profile without constructing a tool execution
/// context. The desktop viewer uses this read-only path; MCP callers keep the
/// context-backed wrapper above so existing behavior is unchanged.
pub fn query_tool_usage_for_profile(
    profile_id: &str,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 1_000) as usize;
    let top = args
        .get("top")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let scope = args
        .get("scope")
        .and_then(Value::as_str)
        .unwrap_or("current_runtime");
    let sort_by = args
        .get("sort_by")
        .and_then(Value::as_str)
        .unwrap_or("calls");
    let include_records = args
        .get("include_records")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_payloads = args
        .get("include_payloads")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let aggregate = args
        .get("aggregate")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_slowest = args
        .get("include_slowest")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_largest = args
        .get("include_largest")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_performance = args
        .get("include_performance")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_bursts = args
        .get("include_bursts")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let include_async_sessions = args
        .get("include_async_sessions")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let burst_idle_ms = args
        .get("burst_idle_ms")
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_BURST_IDLE_MS)
        .clamp(1_000, 3_600_000);
    let errors_only = args
        .get("errors_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let min_duration_ms = args
        .get("min_duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let since_ts_ms = args.get("since_ts_ms").and_then(Value::as_u64).unwrap_or(0);
    let tools = string_filter(args.get("tools"));
    let mut exclude_tools = string_filter(args.get("exclude_tools"));
    if exclude_tools.is_empty() {
        exclude_tools.push("query_tool_usage".into());
    }
    let outcomes = string_filter(args.get("outcomes"));

    let log_dir = crate::tunnel::log_dir_for_profile(profile_id);
    let paths = log_paths(&log_dir);
    let mut invalid_lines = 0u64;
    let mut scanned_lines = 0u64;
    let mut matched_lines = 0u64;
    let mut matched_async_session_events = 0u64;
    let mut recent = VecDeque::with_capacity(limit);
    let mut stats = BTreeMap::<String, ToolStats>::new();
    let mut outcome_counts = BTreeMap::<String, u64>::new();
    let mut error_counts = BTreeMap::<String, u64>::new();
    let mut repeated_identical_error_count = 0u64;
    let mut previous_error_signature: Option<(String, String, String)> = None;
    let mut totals = ToolStats::default();
    let mut slowest = Vec::<Value>::new();
    let mut largest = Vec::<Value>::new();
    let mut performance = PerformanceStats::default();

    for path in paths {
        let (scanned, invalid) = visit_complete_jsonl_records(&path, |record| {
            let event = record
                .get("event")
                .and_then(Value::as_str)
                .unwrap_or("tool_call");
            if event == "async_session_finalized" {
                let exec_requested = tools.is_empty()
                    || tools
                        .iter()
                        .any(|tool| matches!(tool.as_str(), "exec_command" | "exec_many"));
                let exec_excluded = exclude_tools
                    .iter()
                    .any(|tool| matches!(tool.as_str(), "exec_command" | "exec_many"));
                let child_duration = record
                    .get("child_process_total_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                if include_performance
                    && include_async_sessions
                    && exec_requested
                    && !exec_excluded
                    && !errors_only
                    && outcomes.is_empty()
                    && child_duration >= min_duration_ms
                    && matches_scope_and_time(&record, scope, since_ts_ms)
                {
                    matched_async_session_events += 1;
                    accumulate_async_session(&record, &mut performance);
                }
                return;
            }
            if event != "tool_call" {
                return;
            }
            if !matches_filters(
                &record,
                &tools,
                &exclude_tools,
                &outcomes,
                errors_only,
                min_duration_ms,
                since_ts_ms,
                scope,
            ) {
                return;
            }
            matched_lines += 1;
            let current_error_signature = error_signature(&record);
            if current_error_signature.is_some()
                && current_error_signature == previous_error_signature
            {
                repeated_identical_error_count += 1;
            }
            previous_error_signature = current_error_signature;
            if include_performance {
                accumulate_performance(&record, &mut performance, burst_idle_ms);
            }
            if aggregate {
                accumulate(
                    &record,
                    &mut totals,
                    &mut stats,
                    &mut outcome_counts,
                    &mut error_counts,
                );
            }
            if include_slowest {
                push_top_record(&mut slowest, &record, "duration_ms", top);
            }
            if include_largest {
                push_top_record(&mut largest, &record, "response_json_bytes", top);
            }
            if include_records {
                if recent.len() == limit {
                    recent.pop_front();
                }
                recent.push_back(if include_payloads {
                    record
                } else {
                    compact_record(record)
                });
            }
        })?;
        scanned_lines += scanned;
        invalid_lines += invalid;
    }

    let mut tool_stats = stats
        .into_iter()
        .map(|(tool, stats)| {
            json!({
                "tool": tool,
                "calls": stats.calls,
                "errors": stats.errors,
                "warnings": stats.warnings,
                "duration_ms": stats.duration_ms,
                "queue_wait_ms": stats.queue_wait_ms,
                "workspace_admission_wait_ms": stats.workspace_admission_wait_ms,
                "global_admission_wait_ms": stats.global_admission_wait_ms,
                "blocking_queue_wait_ms": stats.blocking_queue_wait_ms,
                "workspace_lock_wait_ms": stats.workspace_lock_wait_ms,
                "history_lock_wait_ms": stats.history_lock_wait_ms,
                "session_registry_wait_ms": stats.session_registry_wait_ms,
                "actual_wait_ms": stats.actual_wait_ms,
                "snapshot_ms": stats.snapshot_ms,
                "resource_lock_wait_ms": stats.resource_lock_wait_ms,
                "operation_lock_wait_ms": stats.operation_lock_wait_ms,
                "batch_queue_wait_ms": stats.batch_queue_wait_ms,
                "queue_nonzero": stats.queue_nonzero,
                "avg_ms": average(stats.duration_ms, stats.calls),
                "p50_ms": percentile(&stats.durations, 50),
                "p95_ms": percentile(&stats.durations, 95),
                "max_ms": stats.durations.iter().copied().max().unwrap_or(0),
                "request_bytes": stats.request_bytes,
                "response_bytes": stats.response_bytes,
                "optimization": {
                    "recovery_actions": stats.recovery_actions,
                    "failed_command_ids": stats.failed_command_ids,
                    "skipped_command_ids": stats.skipped_command_ids,
                    "empty_wait_timeouts": stats.empty_wait_timeouts,
                    "deduplicated_calls": stats.deduplicated_calls,
                    "heartbeat_responses": stats.heartbeat_responses,
                    "detached_responses": stats.detached_responses
                }
                ,"formatting": formatting_stats(&stats)
            })
        })
        .collect::<Vec<_>>();
    tool_stats.sort_by(|a, b| {
        metric_u64(b, sort_by)
            .cmp(&metric_u64(a, sort_by))
            .then_with(|| a["tool"].as_str().cmp(&b["tool"].as_str()))
    });
    tool_stats.truncate(top);

    Ok(tool_ok(json!({
        "workspace_id": profile_id,
        "scope": scope,
        "runtime_boot_id": crate::mcp::runtime_boot_id(),
        "server_version": env!("CARGO_PKG_VERSION"),
        "log_dir": log_dir.display().to_string(),
        "scanned_lines": scanned_lines,
        "matched_lines": matched_lines,
        "matched_async_session_events": matched_async_session_events,
        "invalid_complete_lines": invalid_lines,
        "records": recent,
        "slowest": if include_slowest { Value::Array(slowest) } else { Value::Null },
        "largest": if include_largest { Value::Array(largest) } else { Value::Null },
        "aggregate": if aggregate {
            json!({
                "calls": totals.calls,
                "errors": totals.errors,
                "warnings": totals.warnings,
                "duration_ms": totals.duration_ms,
                "queue_wait_ms": totals.queue_wait_ms,
                "workspace_admission_wait_ms": totals.workspace_admission_wait_ms,
                "global_admission_wait_ms": totals.global_admission_wait_ms,
                "blocking_queue_wait_ms": totals.blocking_queue_wait_ms,
                "workspace_lock_wait_ms": totals.workspace_lock_wait_ms,
                "history_lock_wait_ms": totals.history_lock_wait_ms,
                "session_registry_wait_ms": totals.session_registry_wait_ms,
                "actual_wait_ms": totals.actual_wait_ms,
                "snapshot_ms": totals.snapshot_ms,
                "resource_lock_wait_ms": totals.resource_lock_wait_ms,
                "operation_lock_wait_ms": totals.operation_lock_wait_ms,
                "batch_queue_wait_ms": totals.batch_queue_wait_ms,
                "queue_nonzero": totals.queue_nonzero,
                "avg_ms": average(totals.duration_ms, totals.calls),
                "p50_ms": percentile(&totals.durations, 50),
                "p95_ms": percentile(&totals.durations, 95),
                "max_ms": totals.durations.iter().copied().max().unwrap_or(0),
                "request_bytes": totals.request_bytes,
                "response_bytes": totals.response_bytes,
                "outcomes": outcome_counts,
                "errors_by_code": error_counts,
                "tools": tool_stats
            })
        } else {
            Value::Null
        },
        "optimization": if aggregate {
            json!({
                "recovery_actions": totals.recovery_actions,
                "failed_command_ids": totals.failed_command_ids,
                "skipped_command_ids": totals.skipped_command_ids,
                "empty_wait_timeouts": totals.empty_wait_timeouts,
                "deduplicated_calls": totals.deduplicated_calls,
                "heartbeat_responses": totals.heartbeat_responses,
                "detached_responses": totals.detached_responses,
                "repeated_identical_error_count": repeated_identical_error_count
            })
        } else {
            Value::Null
        },
        "formatting": if aggregate {
            formatting_stats(&totals)
        } else {
            Value::Null
        },
        "performance": if include_performance {
            performance_report(&performance, top, include_bursts, burst_idle_ms)
        } else {
            Value::Null
        },
        "warnings": Vec::<String>::new()
    })))
}

fn log_paths(log_dir: &Path) -> Vec<PathBuf> {
    let mut paths = (1..=MAX_ROTATED_FILES)
        .rev()
        .map(|index| log_dir.join(format!("{TOOL_USAGE_LOG_FILE}.{index}")))
        .collect::<Vec<_>>();
    paths.push(log_dir.join(TOOL_USAGE_LOG_FILE));
    paths
}

fn visit_complete_jsonl_records<F>(path: &Path, mut visit: F) -> Result<(u64, u64), WorkspaceError>
where
    F: FnMut(Value),
{
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok((0, 0)),
        Err(error) => {
            return Err(WorkspaceError::Tool {
                code: "LOG_READ_FAILED",
                message: format!("Unable to read {}: {error}", path.display()),
                category: "runtime",
                retryable: true,
            })
        }
    };
    let mut reader = std::io::BufReader::new(file);
    let mut buffer = Vec::with_capacity(8 * 1024);
    let mut scanned = 0u64;
    let mut invalid = 0u64;
    loop {
        buffer.clear();
        let read = reader
            .read_until(b'\n', &mut buffer)
            .map_err(|error| WorkspaceError::Tool {
                code: "LOG_READ_FAILED",
                message: format!("Unable to read {}: {error}", path.display()),
                category: "runtime",
                retryable: true,
            })?;
        if read == 0 {
            break;
        }
        if !buffer.ends_with(b"\n") {
            break;
        }
        while buffer
            .last()
            .is_some_and(|byte| *byte == b'\n' || *byte == b'\r')
        {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue;
        }
        scanned += 1;
        match serde_json::from_slice::<Value>(&buffer) {
            Ok(record) => visit(record),
            Err(_) => invalid += 1,
        }
    }
    Ok((scanned, invalid))
}

fn string_filter(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn matches_scope_and_time(record: &Value, scope: &str, since_ts_ms: u64) -> bool {
    let started = record
        .get("started_ts_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let scope_matches = match scope {
        "all" => true,
        "current_version" => {
            record.get("server_version").and_then(Value::as_str) == Some(env!("CARGO_PKG_VERSION"))
        }
        _ => {
            record.get("runtime_boot_id").and_then(Value::as_str)
                == Some(crate::mcp::runtime_boot_id())
        }
    };
    scope_matches && started >= since_ts_ms
}

fn matches_filters(
    record: &Value,
    tools: &[String],
    exclude_tools: &[String],
    outcomes: &[String],
    errors_only: bool,
    min_duration_ms: u64,
    since_ts_ms: u64,
    scope: &str,
) -> bool {
    let tool = record.get("tool").and_then(Value::as_str).unwrap_or("");
    let outcome = normalized_outcome(record);
    let duration = record
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    matches_scope_and_time(record, scope, since_ts_ms)
        && (tools.is_empty() || tools.iter().any(|value| value == tool))
        && !exclude_tools.iter().any(|value| value == tool)
        && (outcomes.is_empty() || outcomes.iter().any(|value| value == outcome))
        && (!errors_only || is_error_record(record))
        && duration >= min_duration_ms
}

fn error_signature(record: &Value) -> Option<(String, String, String)> {
    if !is_error_record(record) {
        return None;
    }
    Some((
        record.get("tool").and_then(Value::as_str)?.to_string(),
        record
            .get("arguments_sha256")
            .and_then(Value::as_str)?
            .to_string(),
        record
            .get("error_code")
            .or_else(|| record.get("rpc_error_code"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
    ))
}

fn accumulate(
    record: &Value,
    totals: &mut ToolStats,
    by_tool: &mut BTreeMap<String, ToolStats>,
    outcomes: &mut BTreeMap<String, u64>,
    errors: &mut BTreeMap<String, u64>,
) {
    let tool = record
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let outcome = normalized_outcome(record).to_string();
    *outcomes.entry(outcome.clone()).or_default() += 1;
    let error_code = record
        .get("error_code")
        .or_else(|| record.get("rpc_error_code"))
        .and_then(Value::as_str);
    if let Some(error_code) = error_code {
        *errors.entry(error_code.to_string()).or_default() += 1;
    }
    add_stats(record, totals);
    add_stats(record, by_tool.entry(tool).or_default());
}

fn add_stats(record: &Value, stats: &mut ToolStats) {
    let duration = record
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.calls += 1;
    stats.errors += u64::from(is_error_record(record));
    stats.warnings += record
        .get("warning_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.duration_ms += duration as u128;
    let queue_wait = record
        .get("admission_queue_wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        + record
            .get("blocking_queue_wait_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
    stats.queue_wait_ms += queue_wait as u128;
    stats.workspace_admission_wait_ms += metric(record, "workspace_admission_wait_ms");
    stats.global_admission_wait_ms += metric(record, "global_admission_wait_ms");
    stats.blocking_queue_wait_ms += metric(record, "blocking_queue_wait_ms");
    stats.workspace_lock_wait_ms += metric(record, "workspace_lock_wait_ms");
    stats.history_lock_wait_ms += metric(record, "history_lock_wait_ms");
    stats.session_registry_wait_ms += metric(record, "session_registry_wait_ms");
    stats.actual_wait_ms += metric(record, "actual_wait_ms");
    stats.snapshot_ms += metric(record, "snapshot_ms");
    stats.resource_lock_wait_ms += metric(record, "resource_lock_wait_ms");
    stats.operation_lock_wait_ms += metric(record, "operation_lock_wait_ms");
    stats.batch_queue_wait_ms += metric(record, "batch_queue_wait_ms");
    stats.queue_nonzero += u64::from(queue_wait > 0);
    stats.request_bytes += record
        .get("request_json_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.response_bytes += record
        .get("response_json_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.recovery_actions += record
        .get("recovery_action_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.failed_command_ids += record
        .get("failed_command_id_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.skipped_command_ids += record
        .get("skipped_command_id_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.empty_wait_timeouts += u64::from(
        record.get("tool").and_then(Value::as_str) == Some("wait_command")
            && record.get("request_timed_out").and_then(Value::as_bool) == Some(true)
            && record
                .get("event_count")
                .and_then(Value::as_u64)
                .unwrap_or(0)
                == 0,
    );
    stats.deduplicated_calls +=
        u64::from(record.get("deduplicated").and_then(Value::as_bool) == Some(true));
    stats.heartbeat_responses +=
        u64::from(record.get("heartbeat").and_then(Value::as_bool) == Some(true));
    stats.detached_responses +=
        u64::from(record.get("detached").and_then(Value::as_bool) == Some(true));
    stats.format_files_requested += record
        .get("format_files_requested")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.format_files_supported += record
        .get("format_files_supported")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.format_files_changed += record
        .get("format_files_changed_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.format_files_unchanged += record
        .get("format_files_unchanged_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.format_files_skipped += record
        .get("format_files_skipped_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.formatter_groups += record
        .get("format_formatter_group_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.custom_formatter_groups += record
        .get("format_custom_formatter_group_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.unavailable_adapters += record
        .get("format_unavailable_adapter_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.unexpected_changes += record
        .get("format_unexpected_change_count")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.format_diff_bytes += record
        .get("format_diff_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    stats.format_apply_calls +=
        u64::from(record.get("format_applied").and_then(Value::as_bool) == Some(true));
    stats.durations.push(duration);
}

fn formatting_stats(stats: &ToolStats) -> Value {
    json!({
        "files_requested": stats.format_files_requested,
        "files_supported": stats.format_files_supported,
        "files_changed": stats.format_files_changed,
        "files_unchanged": stats.format_files_unchanged,
        "files_skipped": stats.format_files_skipped,
        "formatter_groups": stats.formatter_groups,
        "custom_formatter_groups": stats.custom_formatter_groups,
        "unavailable_adapters": stats.unavailable_adapters,
        "unexpected_changes": stats.unexpected_changes,
        "diff_bytes": stats.format_diff_bytes,
        "apply_calls": stats.format_apply_calls
    })
}

fn metric(record: &Value, name: &str) -> u128 {
    record.get(name).and_then(Value::as_u64).unwrap_or(0) as u128
}

fn accumulate_performance(record: &Value, performance: &mut PerformanceStats, burst_idle_ms: u64) {
    let started = record
        .get("started_ts_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let duration = record
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completed = record
        .get("completed_ts_ms")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| started.saturating_add(duration));
    let queue_wait = record
        .get("admission_queue_wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .saturating_add(
            record
                .get("blocking_queue_wait_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0),
        );
    let recorded_gap = record.get("orchestration_gap_ms").and_then(Value::as_u64);
    let concurrent_request = record
        .get("concurrent_request")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let derived_gap = (!concurrent_request && performance.previous_completed_ts_ms > 0)
        .then(|| started.saturating_sub(performance.previous_completed_ts_ms));
    let gap = if concurrent_request {
        None
    } else {
        recorded_gap.or(derived_gap)
    };

    performance.tool_calls += 1;
    performance.server_duration_ms += duration as u128;
    performance.queue_wait_ms += queue_wait as u128;
    if performance.first_started_ts_ms == 0 || started < performance.first_started_ts_ms {
        performance.first_started_ts_ms = started;
    }
    performance.last_completed_ts_ms = performance.last_completed_ts_ms.max(completed);
    performance.previous_completed_ts_ms = performance.previous_completed_ts_ms.max(completed);

    if let Some(gap) = gap {
        if gap > burst_idle_ms {
            performance.idle_gap_ms += gap as u128;
            performance.idle_gap_count += 1;
        } else {
            performance.active_orchestration_gap_ms += gap as u128;
            performance.orchestration_gaps.push(gap);
        }
    }

    if performance.inferred_burst_id == 0 {
        performance.inferred_burst_id = 1;
    } else if gap.is_some_and(|gap| gap > burst_idle_ms) {
        performance.inferred_burst_id = performance.inferred_burst_id.saturating_add(1);
    }
    let runtime_boot_id = record
        .get("runtime_boot_id")
        .and_then(Value::as_str)
        .unwrap_or("legacy");
    let burst_id = record
        .get("activity_burst_id")
        .and_then(Value::as_u64)
        .unwrap_or(performance.inferred_burst_id);
    let burst_key = format!("{runtime_boot_id}:{burst_id}");
    let burst = performance.bursts.entry(burst_key).or_default();
    burst.calls += 1;
    if burst.first_started_ts_ms == 0 || started < burst.first_started_ts_ms {
        burst.first_started_ts_ms = started;
    }
    burst.last_completed_ts_ms = burst.last_completed_ts_ms.max(completed);
    burst.server_duration_ms += duration as u128;
    if let Some(gap) = gap.filter(|gap| *gap <= burst_idle_ms) {
        burst.orchestration_gap_ms += gap as u128;
    }
    let tool = record
        .get("tool")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    *burst.tools.entry(tool.to_string()).or_default() += 1;

    if let Some(kind) = record.get("command_kind").and_then(Value::as_str) {
        let kind_stats = performance
            .command_kinds
            .entry(kind.to_string())
            .or_default();
        kind_stats.calls += 1;
        kind_stats.server_duration_ms += duration as u128;
    }
}

fn accumulate_async_session(record: &Value, performance: &mut PerformanceStats) {
    let started = record
        .get("started_ts_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let completed = record
        .get("completed_ts_ms")
        .and_then(Value::as_u64)
        .unwrap_or(started);
    let child_process_ms = record
        .get("child_process_total_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    performance.async_sessions += 1;
    if performance.first_started_ts_ms == 0 || started < performance.first_started_ts_ms {
        performance.first_started_ts_ms = started;
    }
    performance.last_completed_ts_ms = performance.last_completed_ts_ms.max(completed);
    performance.child_process_ms += child_process_ms as u128;
    performance.child_durations.push(child_process_ms);
    if let Some(first_output_ms) = record.get("first_output_ms").and_then(Value::as_u64) {
        performance.first_output_durations.push(first_output_ms);
    }
    if record
        .get("exit_code")
        .and_then(Value::as_i64)
        .is_some_and(|exit_code| exit_code != 0)
    {
        performance.child_failures += 1;
    }
    let termination = record
        .get("termination_reason")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    *performance
        .child_terminations
        .entry(termination.to_string())
        .or_default() += 1;
    let kind = record
        .get("command_kind")
        .and_then(Value::as_str)
        .unwrap_or("process");
    let kind_stats = performance
        .command_kinds
        .entry(kind.to_string())
        .or_default();
    kind_stats.child_sessions += 1;
    kind_stats.child_process_ms += child_process_ms as u128;
}

fn performance_report(
    performance: &PerformanceStats,
    top: usize,
    include_bursts: bool,
    burst_idle_ms: u64,
) -> Value {
    let observed_wall_ms = performance
        .last_completed_ts_ms
        .saturating_sub(performance.first_started_ts_ms);
    let attributed_nonoverlap = performance
        .server_duration_ms
        .saturating_add(performance.active_orchestration_gap_ms);
    let server_share = percentage(performance.server_duration_ms, attributed_nonoverlap);
    let orchestration_share = percentage(
        performance.active_orchestration_gap_ms,
        attributed_nonoverlap,
    );
    let dominant = if performance.active_orchestration_gap_ms > performance.server_duration_ms {
        "client_orchestration_gap"
    } else if performance.server_duration_ms > 0 {
        "server_tool_execution"
    } else {
        "insufficient_data"
    };

    let mut command_kinds = performance
        .command_kinds
        .iter()
        .map(|(kind, stats)| {
            json!({
                "command_kind": kind,
                "calls": stats.calls,
                "server_duration_ms": stats.server_duration_ms,
                "child_sessions": stats.child_sessions,
                "child_process_ms": stats.child_process_ms
            })
        })
        .collect::<Vec<_>>();
    command_kinds.sort_by(|a, b| {
        let a_total = a["server_duration_ms"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(a["child_process_ms"].as_u64().unwrap_or(0));
        let b_total = b["server_duration_ms"]
            .as_u64()
            .unwrap_or(0)
            .saturating_add(b["child_process_ms"].as_u64().unwrap_or(0));
        b_total.cmp(&a_total)
    });
    command_kinds.truncate(top);

    let bursts = if include_bursts {
        let mut bursts = performance
            .bursts
            .iter()
            .map(|(burst_id, burst)| {
                json!({
                    "burst_id": burst_id,
                    "calls": burst.calls,
                    "started_ts_ms": burst.first_started_ts_ms,
                    "completed_ts_ms": burst.last_completed_ts_ms,
                    "wall_ms": burst.last_completed_ts_ms.saturating_sub(burst.first_started_ts_ms),
                    "server_duration_ms": burst.server_duration_ms,
                    "orchestration_gap_ms": burst.orchestration_gap_ms,
                    "tools": burst.tools
                })
            })
            .collect::<Vec<_>>();
        bursts.sort_by(|a, b| {
            b["started_ts_ms"]
                .as_u64()
                .unwrap_or(0)
                .cmp(&a["started_ts_ms"].as_u64().unwrap_or(0))
        });
        bursts.truncate(top);
        Value::Array(bursts)
    } else {
        Value::Null
    };

    json!({
        "tool_calls": performance.tool_calls,
        "async_sessions_finalized": performance.async_sessions,
        "observed_wall_ms": observed_wall_ms,
        "server_tool_duration_ms": performance.server_duration_ms,
        "server_queue_wait_ms": performance.queue_wait_ms,
        "client_orchestration_gap_ms": performance.active_orchestration_gap_ms,
        "client_orchestration_gap_p50_ms": percentile(&performance.orchestration_gaps, 50),
        "client_orchestration_gap_p95_ms": percentile(&performance.orchestration_gaps, 95),
        "idle_gap_ms": performance.idle_gap_ms,
        "idle_gap_count": performance.idle_gap_count,
        "burst_idle_threshold_ms": burst_idle_ms,
        "child_process_lifetime_ms": performance.child_process_ms,
        "child_process_failures": performance.child_failures,
        "child_process_terminations": performance.child_terminations,
        "child_process_p50_ms": percentile(&performance.child_durations, 50),
        "child_process_p95_ms": percentile(&performance.child_durations, 95),
        "first_output_p50_ms": percentile(&performance.first_output_durations, 50),
        "first_output_p95_ms": percentile(&performance.first_output_durations, 95),
        "server_share_of_nonidle_attributed_percent": server_share,
        "client_orchestration_share_of_nonidle_attributed_percent": orchestration_share,
        "dominant_observed_nonidle_source": dominant,
        "command_kinds": command_kinds,
        "activity_bursts": bursts,
        "attribution_note": "client_orchestration_gap is observed between the previous tool response completing and the next tool request arriving. It includes model reasoning, platform scheduling, connector/network latency, and client-side orchestration; it is not pure LLM inference time. Child-process lifetime may overlap both server duration and orchestration gaps and must not be added as an independent wall-time component."
    })
}

fn percentage(value: u128, total: u128) -> f64 {
    if total == 0 {
        0.0
    } else {
        value as f64 * 100.0 / total as f64
    }
}

fn normalized_outcome(record: &Value) -> &str {
    if let Some(outcome) = record.get("outcome").and_then(Value::as_str) {
        return outcome;
    }
    if record.get("is_error").and_then(Value::as_bool) == Some(true)
        || record.get("ok").and_then(Value::as_bool) == Some(false)
    {
        "legacy_error"
    } else if record.get("ok").and_then(Value::as_bool) == Some(true) {
        "success"
    } else {
        "legacy_unknown"
    }
}

fn is_error_record(record: &Value) -> bool {
    if record.get("is_error").and_then(Value::as_bool) == Some(true) {
        return true;
    }
    if record.get("ok").and_then(Value::as_bool) == Some(false) {
        return true;
    }
    matches!(
        normalized_outcome(record),
        "rpc_error" | "tool_error" | "worker_failed" | "legacy_error"
    )
}

fn metric_u64(record: &Value, field: &str) -> u64 {
    match field {
        "p95_ms" => record.get("p95_ms").and_then(Value::as_u64).unwrap_or(0),
        "errors" => record.get("errors").and_then(Value::as_u64).unwrap_or(0),
        "duration_ms" => record
            .get("duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "response_bytes" => record
            .get("response_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "request_bytes" => record
            .get("request_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        "queue_wait_ms" => record
            .get("queue_wait_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        _ => record.get("calls").and_then(Value::as_u64).unwrap_or(0),
    }
}

fn push_top_record(records: &mut Vec<Value>, record: &Value, field: &str, top: usize) {
    records.push(compact_record(record.clone()));
    records.sort_by(|a, b| {
        b.get(field)
            .and_then(Value::as_u64)
            .unwrap_or(0)
            .cmp(&a.get(field).and_then(Value::as_u64).unwrap_or(0))
    });
    records.truncate(top);
}

fn compact_record(mut record: Value) -> Value {
    if let Some(object) = record.as_object_mut() {
        for field in [
            "arguments",
            "arguments_json",
            "argument_field_bytes",
            "stdout",
            "stderr",
        ] {
            object.remove(field);
        }
    }
    record
}

fn average(duration_ms: u128, calls: u64) -> f64 {
    if calls == 0 {
        0.0
    } else {
        duration_ms as f64 / calls as f64
    }
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() - 1) * percentile).div_ceil(100);
    sorted[index.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentile_is_bounded_and_stable() {
        assert_eq!(percentile(&[], 95), 0);
        assert_eq!(percentile(&[1, 2, 3, 4, 100], 50), 3);
        assert_eq!(percentile(&[1, 2, 3, 4, 100], 95), 100);
    }

    #[test]
    fn incomplete_jsonl_tail_is_ignored() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("usage.jsonl");
        std::fs::write(
            &path,
            b"{\"tool\":\"server_info\",\"outcome\":\"success\"}\n{\"tool\":",
        )
        .expect("write log");
        let mut records = Vec::new();
        let (scanned, invalid) =
            visit_complete_jsonl_records(&path, |record| records.push(record)).expect("read log");
        assert_eq!(scanned, 1);
        assert_eq!(invalid, 0);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["tool"], "server_info");
    }

    #[test]
    fn error_signature_identifies_exact_retries_only() {
        let failed = json!({
            "tool": "edit_file",
            "outcome": "tool_error",
            "arguments_sha256": "abc",
            "error_code": "FILE_VERSION_MISMATCH"
        });
        let same = failed.clone();
        let different = json!({
            "tool": "edit_file",
            "outcome": "tool_error",
            "arguments_sha256": "def",
            "error_code": "FILE_VERSION_MISMATCH"
        });
        assert_eq!(error_signature(&failed), error_signature(&same));
        assert_ne!(error_signature(&failed), error_signature(&different));
        assert_eq!(
            error_signature(&json!({"tool": "read_file", "outcome": "success"})),
            None
        );
    }

    #[test]
    fn command_coordination_metrics_are_aggregated() {
        let mut stats = ToolStats::default();
        add_stats(
            &json!({
                "tool": "exec_command",
                "duration_ms": 40,
                "operation_lock_wait_ms": 7,
                "resource_lock_wait_ms": 13,
                "deduplicated": true,
                "heartbeat": true,
                "detached": true
            }),
            &mut stats,
        );

        assert_eq!(stats.operation_lock_wait_ms, 7);
        assert_eq!(stats.resource_lock_wait_ms, 13);
        assert_eq!(stats.deduplicated_calls, 1);
        assert_eq!(stats.heartbeat_responses, 1);
        assert_eq!(stats.detached_responses, 1);
    }

    #[test]
    fn format_metrics_are_aggregated() {
        let mut stats = ToolStats::default();
        add_stats(
            &json!({
                "tool": "format_files",
                "duration_ms": 25,
                "format_files_requested": 6,
                "format_files_supported": 5,
                "format_files_changed_count": 2,
                "format_files_unchanged_count": 3,
                "format_files_skipped_count": 1,
                "format_formatter_group_count": 3,
                "format_custom_formatter_group_count": 1,
                "format_unavailable_adapter_count": 1,
                "format_unexpected_change_count": 0,
                "format_diff_bytes": 512,
                "format_applied": true
            }),
            &mut stats,
        );

        assert_eq!(stats.format_files_requested, 6);
        assert_eq!(stats.format_files_supported, 5);
        assert_eq!(stats.format_files_changed, 2);
        assert_eq!(stats.format_files_unchanged, 3);
        assert_eq!(stats.format_files_skipped, 1);
        assert_eq!(stats.formatter_groups, 3);
        assert_eq!(stats.custom_formatter_groups, 1);
        assert_eq!(stats.unavailable_adapters, 1);
        assert_eq!(stats.unexpected_changes, 0);
        assert_eq!(stats.format_diff_bytes, 512);
        assert_eq!(stats.format_apply_calls, 1);

        let value = formatting_stats(&stats);
        assert_eq!(value["files_requested"], 6);
        assert_eq!(value["files_changed"], 2);
        assert_eq!(value["custom_formatter_groups"], 1);
        assert_eq!(value["apply_calls"], 1);
    }

    #[test]
    fn performance_report_separates_server_gaps_and_child_lifetimes() {
        let mut performance = PerformanceStats::default();
        accumulate_performance(
            &json!({
                "event": "tool_call",
                "runtime_boot_id": "boot",
                "activity_burst_id": 1,
                "tool": "exec_command",
                "command_kind": "cargo_test",
                "started_ts_ms": 1_000,
                "completed_ts_ms": 1_200,
                "duration_ms": 200,
                "orchestration_gap_ms": null
            }),
            &mut performance,
            120_000,
        );
        accumulate_performance(
            &json!({
                "event": "tool_call",
                "runtime_boot_id": "boot",
                "activity_burst_id": 1,
                "tool": "read_output",
                "started_ts_ms": 3_000,
                "completed_ts_ms": 3_010,
                "duration_ms": 10,
                "orchestration_gap_ms": 1_800
            }),
            &mut performance,
            120_000,
        );
        accumulate_async_session(
            &json!({
                "event": "async_session_finalized",
                "command_kind": "cargo_test",
                "child_process_total_ms": 5_000,
                "first_output_ms": 400
            }),
            &mut performance,
        );

        let report = performance_report(&performance, 20, true, 120_000);
        assert_eq!(report["server_tool_duration_ms"], 210);
        assert_eq!(report["client_orchestration_gap_ms"], 1_800);
        assert_eq!(report["child_process_lifetime_ms"], 5_000);
        assert_eq!(report["first_output_p50_ms"], 400);
        assert_eq!(
            report["dominant_observed_nonidle_source"],
            "client_orchestration_gap"
        );
        assert_eq!(report["command_kinds"][0]["command_kind"], "cargo_test");
        assert_eq!(report["command_kinds"][0]["child_sessions"], 1);
        assert_eq!(report["activity_bursts"][0]["calls"], 2);
    }
}
