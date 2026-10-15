//! Session command handlers behind the stable public session facade.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::AsyncWriteExt;

use crate::tools::workspace::{tool_ok, WorkspaceError};

use super::output::{
    align_output_start, bounded_output_end, decode_process_output_with_encoding, EventDetail,
    OutputMode, OutputOptions,
};
#[cfg(windows)]
use super::process_lifecycle::terminate_process;
use super::{
    ExecSession, SessionStore, FINALIZED_SESSION_RETENTION, WAIT_COMMAND_TIMEOUT_DEFAULT_MS,
    WAIT_COMMAND_TIMEOUT_MAX_MS, WAIT_COMMAND_TRANSPORT_SAFE_MS,
};

pub(super) fn run_read_output(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(run_read_output_async(store, args))
}

pub(super) async fn run_read_output_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let output_ref = args
        .get("output_ref")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("output_ref is required"))?;
    let Some(rest) = output_ref.strip_prefix("output://") else {
        return Err(WorkspaceError::invalid_argument(
            "output_ref must look like output://<session-id>/stdout or output://<session-id>/stderr",
        ));
    };
    let Some((session_id, ref_stream)) = rest.rsplit_once('/') else {
        return Err(WorkspaceError::invalid_argument(
            "output_ref must include a stream suffix",
        ));
    };
    if ref_stream != "stdout" && ref_stream != "stderr" {
        return Err(WorkspaceError::invalid_argument(
            "output_ref stream must be stdout or stderr",
        ));
    }
    let session = store.get(session_id)?;
    session.touch_attachment();
    session.refresh_status().await;

    let stream = ref_stream;

    let snapshot = session.stream_snapshot(stream);
    let data = snapshot.data;
    let total_stream_bytes = snapshot.total_bytes;
    let encoding = snapshot.encoding;
    let retained_start = total_stream_bytes.saturating_sub(data.len());
    let requested_offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let requested_local_offset = requested_offset
        .max(retained_start)
        .min(total_stream_bytes)
        .saturating_sub(retained_start)
        .min(data.len());
    let local_offset = align_output_start(&data, requested_local_offset, encoding, retained_start);
    let effective_offset = retained_start.saturating_add(local_offset);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(4096)
        .clamp(1, 1_048_576) as usize;
    let local_end = bounded_output_end(&data, local_offset, limit, encoding);
    let chunk = &data[local_offset..local_end];
    let absolute_end = retained_start.saturating_add(local_end);
    let next_offset = (absolute_end < total_stream_bytes).then_some(absolute_end as u64);

    Ok(tool_ok(json!({
        "output_ref": output_ref,
        "stream_output_ref": format!("output://{session_id}/{stream}"),
        "stream": stream,
        "offset": effective_offset,
        "requested_offset": requested_offset,
        "retained_start_offset": retained_start,
        "cursor_expired": requested_offset < retained_start,
        "limit": limit,
        "encoding": encoding.as_str(),
        "content": decode_process_output_with_encoding(chunk, encoding),
        "next_offset": next_offset,
        "total_retained_bytes": data.len(),
        "total_stream_bytes": total_stream_bytes,
        "truncated": next_offset.is_some(),
        "warnings": if requested_offset < retained_start {
            vec!["requested offset expired; response starts at the oldest retained byte"]
        } else if effective_offset != requested_offset {
            vec!["requested offset was aligned to the start of a complete character"]
        } else {
            Vec::<&str>::new()
        }
    })))
}

const MANAGED_JOB_POLL_FAST_MS: u64 = 2_000;
const MANAGED_JOB_POLL_SHORT_MS: u64 = 5_000;
const MANAGED_JOB_POLL_MEDIUM_MS: u64 = 15_000;
const MANAGED_JOB_POLL_LONG_MS: u64 = 30_000;

fn managed_job_poll_after_ms(session: &ExecSession, made_progress: bool) -> u64 {
    if session.is_finalized() {
        return 0;
    }
    if made_progress {
        return MANAGED_JOB_POLL_FAST_MS;
    }
    match session.started_at.elapsed().as_millis() as u64 {
        0..=29_999 => MANAGED_JOB_POLL_SHORT_MS,
        30_000..=119_999 => MANAGED_JOB_POLL_MEDIUM_MS,
        _ => MANAGED_JOB_POLL_LONG_MS,
    }
}

fn recovered_operation_payload(operation_id: &str, record: &Value) -> Value {
    let current_boot_id = crate::mcp::runtime_boot_id();
    let previous_boot_id = record
        .get("runtime_boot_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let finalized = record
        .get("finalized")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut payload = record.get("snapshot").cloned().unwrap_or_else(|| json!({}));
    if let Some(object) = payload.as_object_mut() {
        object.insert("operation_id".into(), json!(operation_id));
        object.insert("resolved_by".into(), json!("durable_operation_record"));
        object.insert("deduplicated".into(), Value::Bool(true));
        object.insert("attached_to_session_id".into(), Value::Null);
        object.insert("restored_from_disk".into(), Value::Bool(true));
        object.insert("runtime_session_available".into(), Value::Bool(false));
        object.insert("previous_runtime_boot_id".into(), json!(previous_boot_id));
        object.insert("current_runtime_boot_id".into(), json!(current_boot_id));
        object.insert("output_refs".into(), Value::Null);
        object.insert("events".into(), json!([]));
        object.insert("stdout".into(), Value::String(String::new()));
        object.insert("stderr".into(), Value::String(String::new()));
        object.insert("next_actions".into(), json!([]));
        if finalized {
            object.insert(
                "suggestion".into(),
                json!("Recovered finalized managed-job metadata after runtime restart. Raw retained output is unavailable after restart."),
            );
        } else {
            let reason = if previous_boot_id == current_boot_id {
                "session_registry_evicted"
            } else {
                "server_restart"
            };
            object.insert("status".into(), json!("runtime_restarted"));
            object.insert("termination_reason".into(), json!(reason));
            object.insert("process_still_running".into(), Value::Bool(false));
            object.insert("recoverable".into(), Value::Bool(true));
            object.insert("retry_allowed".into(), Value::Bool(true));
            object.insert("execution_ok".into(), Value::Null);
            object.insert("verification_ok".into(), Value::Null);
            object.insert("command_ok".into(), Value::Null);
            object.insert(
                "suggestion".into(),
                json!("The prior runtime ended before this managed job finalized. Inspect workspace state, then start a replacement operation with a new operation_id and optionally correlate it with recovery_of_operation_id."),
            );
        }
    }
    payload
}

pub(super) fn run_resolve_operation(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(run_resolve_operation_async(store, args))
}

pub(super) async fn run_resolve_operation_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let operation_id = args.get("operation_id").and_then(Value::as_str);
    let fingerprint = args.get("command_fingerprint").and_then(Value::as_str);
    let (session, resolved_by) = if let Some(operation_id) = operation_id {
        (store.get_by_operation(operation_id), "operation_id")
    } else if let Some(fingerprint) = fingerprint {
        (store.get_by_fingerprint(fingerprint), "command_fingerprint")
    } else {
        return Err(WorkspaceError::invalid_argument(
            "operation_id or command_fingerprint is required",
        ));
    };
    let session = match session {
        Some(session) => session,
        None => {
            if let Some(operation_id) = operation_id {
                if let Some(record) = store.load_persisted_operation(operation_id) {
                    return Ok(tool_ok(recovered_operation_payload(operation_id, &record)));
                }
            }
            return Err(WorkspaceError::ToolDetails {
                code: "OPERATION_NOT_FOUND",
                message: "No retained command session matches the requested operation.".into(),
                category: "not_found",
                retryable: false,
                details: json!({
                    "operation_id": operation_id,
                    "command_fingerprint": fingerprint,
                    "retention_seconds": FINALIZED_SESSION_RETENTION.as_secs(),
                    "suggestion": "Use list_sessions to inspect retained commands before starting a replacement process."
                }),
            });
        }
    };
    session.touch_attachment();
    session.refresh_status().await;
    let mut payload =
        session.snapshot_with_options(OutputOptions::from_args(args, OutputMode::Tail));
    let managed_job_operation = if !session.is_finalized()
        && payload.get("execution_mode").and_then(Value::as_str) == Some("job")
    {
        session.operation_id().map(str::to_string)
    } else {
        None
    };
    let next_cursor = payload
        .get("next_cursor")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let requested_cursor = args.get("cursor").and_then(Value::as_u64).unwrap_or(0);
    let made_progress = next_cursor > requested_cursor;
    let recommended_poll_after_ms = managed_job_operation
        .as_ref()
        .map(|_| managed_job_poll_after_ms(&session, made_progress));
    if let Some(object) = payload.as_object_mut() {
        object.insert("resolved_by".into(), json!(resolved_by));
        object.insert("deduplicated".into(), Value::Bool(true));
        object.insert(
            "attached_to_session_id".into(),
            Value::String(session.session_id.clone()),
        );
        if let Some(operation_id) = managed_job_operation {
            object.insert(
                "next_actions".into(),
                json!([{
                    "tool": "resolve_operation",
                    "after_ms": recommended_poll_after_ms.unwrap_or(MANAGED_JOB_POLL_SHORT_MS),
                    "arguments": {
                        "operation_id": operation_id,
                        "cursor": next_cursor,
                        "output_mode": "delta",
                        "event_detail": "compact"
                    }
                }]),
            );
            object.insert(
                "recommended_poll_after_ms".into(),
                json!(recommended_poll_after_ms.unwrap_or(MANAGED_JOB_POLL_SHORT_MS)),
            );
            object.insert("poll_strategy".into(), json!("managed_job_backoff"));
            object.insert(
                "suggestion".into(),
                json!("Managed job is still active; wait recommended_poll_after_ms, then use next_actions for a non-blocking snapshot."),
            );
        }
    }
    Ok(tool_ok(payload))
}

pub(super) fn run_list_sessions(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let include_finalized = args
        .get("include_finalized")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let limit = args
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(100)
        .clamp(1, 1000) as usize;
    let status_filter = args.get("status").and_then(Value::as_str);
    let sessions = store
        .list(include_finalized, limit)
        .into_iter()
        .map(|session| session.summary())
        .filter(|summary| {
            status_filter.map_or(true, |status| {
                summary.get("status").and_then(Value::as_str) == Some(status)
            })
        })
        .collect::<Vec<_>>();
    let count = sessions.len();
    Ok(tool_ok(json!({
        "sessions": sessions,
        "count": count,
        "include_finalized": include_finalized,
        "retention_seconds": FINALIZED_SESSION_RETENTION.as_secs()
    })))
}

pub(super) fn run_wait_command(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(run_wait_command_async(store, args))
}

pub(super) async fn run_wait_command_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let session_id = required_session_id(args)?;
    let (session, session_registry_wait_ms) = store.get_with_metrics(session_id)?;
    session.touch_attachment();
    let timeout_ms = args
        .get("timeout_ms")
        .and_then(Value::as_u64)
        .unwrap_or(WAIT_COMMAND_TIMEOUT_DEFAULT_MS)
        .min(WAIT_COMMAND_TIMEOUT_MAX_MS);
    let heartbeat_ms = args
        .get("heartbeat_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(30_000);
    let effective_wait_ms = timeout_ms.min(WAIT_COMMAND_TRANSPORT_SAFE_MS);
    let until = args
        .get("until")
        .and_then(Value::as_str)
        .unwrap_or("output_or_exit");
    let mut output_args = args.clone();
    if output_args.get("event_detail").is_none() {
        output_args["event_detail"] = json!("compact");
    }
    let options = OutputOptions::from_args(&output_args, OutputMode::Delta);
    let actual_wait_started = Instant::now();
    let changed = session
        .wait_for_change(
            options.cursor,
            Duration::from_millis(effective_wait_ms),
            until,
        )
        .await;
    let actual_wait_ms = actual_wait_started.elapsed().as_millis();
    let snapshot_started = Instant::now();
    let mut payload = session.snapshot_with_options(options);
    let snapshot_ms = snapshot_started.elapsed().as_millis();
    let request_timed_out = !changed && !session.wait_condition_satisfied(options.cursor, until);
    if let Some(object) = payload.as_object_mut() {
        let process_still_running = object
            .get("process_still_running")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let post_checks_pending = object
            .get("post_checks_pending")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let observation_pending = process_still_running || post_checks_pending;
        let observation_timed_out = request_timed_out && observation_pending;
        let observation_state = if !observation_pending {
            "completed"
        } else if post_checks_pending {
            "verifying"
        } else if request_timed_out {
            "working"
        } else {
            "progress"
        };
        let managed_job_operation =
            if object.get("execution_mode").and_then(Value::as_str) == Some("job") {
                object
                    .get("operation_id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            } else {
                None
            };
        let heartbeat = false;
        let next_cursor = object
            .get("next_cursor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let made_progress = next_cursor > options.cursor;
        let recommended_poll_after_ms = managed_job_operation
            .as_ref()
            .map(|_| managed_job_poll_after_ms(&session, made_progress));
        object.insert(
            "session_registry_wait_ms".into(),
            json!(session_registry_wait_ms),
        );
        object.insert("actual_wait_ms".into(), json!(actual_wait_ms));
        object.insert("snapshot_ms".into(), json!(snapshot_ms));
        object.insert("heartbeat".into(), Value::Bool(heartbeat));
        object.insert("request_timed_out".into(), Value::Bool(request_timed_out));
        object.insert(
            "observation_timed_out".into(),
            Value::Bool(observation_timed_out),
        );
        object.insert("observation_state".into(), json!(observation_state));
        object.insert(
            "transport_safe_wait_ms".into(),
            json!(WAIT_COMMAND_TRANSPORT_SAFE_MS),
        );
        object.insert("wait_timeout_ms".into(), json!(timeout_ms));
        object.insert("effective_wait_ms".into(), json!(effective_wait_ms));
        object.insert("heartbeat_ms".into(), json!(heartbeat_ms));
        object.insert("wait_until".into(), json!(until));
        if let Some(poll_after_ms) = recommended_poll_after_ms {
            object.insert("recommended_poll_after_ms".into(), json!(poll_after_ms));
            object.insert("poll_strategy".into(), json!("managed_job_backoff"));
        }
        if observation_pending {
            let next_action = if let Some(operation_id) = managed_job_operation.as_deref() {
                json!({
                    "tool": "resolve_operation",
                    "after_ms": recommended_poll_after_ms.unwrap_or(MANAGED_JOB_POLL_SHORT_MS),
                    "arguments": {
                        "operation_id": operation_id,
                        "cursor": next_cursor,
                        "output_mode": "delta",
                        "event_detail": "compact"
                    }
                })
            } else {
                json!({
                    "tool": "wait_command",
                    "arguments": {
                        "session_id": session_id,
                        "cursor": next_cursor,
                        "timeout_ms": if timeout_ms == 0 { WAIT_COMMAND_TIMEOUT_DEFAULT_MS } else { effective_wait_ms },
                        "until": until,
                        "output_mode": "delta",
                        "event_detail": options.event_detail.as_str()
                    }
                })
            };
            object.insert("next_actions".into(), json!([next_action]));
        }
        object.insert(
            "suggestion".into(),
            json!(if request_timed_out && managed_job_operation.is_some() {
                "本次观察窗口结束；长任务仍在执行，后续沿用 next_actions 的 resolve_operation 做非阻塞快照"
            } else if request_timed_out && observation_pending {
                "本次观察窗口结束，但既有 session 仍在执行；沿用 next_actions，不要重新调用 exec_command"
            } else if process_still_running {
                "已收到增量输出；沿用 next_actions 继续既有 session"
            } else if post_checks_pending {
                "进程已退出但验证尚未完成；沿用 next_actions 等待 finalized"
            } else {
                "进程已结束；检查 process_exit_code 与 post_checks"
            }),
        );
    }
    Ok(tool_ok(payload))
}

pub(super) fn run_send_input(store: &SessionStore, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(run_send_input_async(store, args))
}

pub(super) async fn run_send_input_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let session_id = required_session_id(args)?;
    let session = store.get(session_id)?;
    session.touch_attachment();
    let chars = args.get("chars").and_then(Value::as_str).unwrap_or("");
    let close_stdin = args
        .get("close_stdin")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if !session.is_running().await {
        return Err(WorkspaceError::Tool {
            code: "SESSION_CLOSED",
            message: "Session is closed; stdin write blocked.".into(),
            category: "runtime",
            retryable: false,
        });
    }

    let bytes_written = async {
        let mut stdin_guard = session.stdin.lock().await;
        let stdin = stdin_guard.as_mut().ok_or_else(|| WorkspaceError::Tool {
            code: "SESSION_CLOSED",
            message: "Session stdin is closed.".into(),
            category: "runtime",
            retryable: false,
        })?;
        if !chars.is_empty() {
            stdin
                .write_all(chars.as_bytes())
                .await
                .map_err(|_| WorkspaceError::Tool {
                    code: "SESSION_CLOSED",
                    message: "Session stdin is closed.".into(),
                    category: "runtime",
                    retryable: false,
                })?;
            stdin.flush().await.map_err(|_| WorkspaceError::Tool {
                code: "SESSION_CLOSED",
                message: "Session stdin is closed.".into(),
                category: "runtime",
                retryable: false,
            })?;
        }
        if close_stdin {
            let _ = stdin.shutdown().await;
            *stdin_guard = None;
            session.mark_stdin_closed();
        }
        Ok::<usize, WorkspaceError>(chars.len())
    }
    .await?;

    let mut payload = session.snapshot_with_options(OutputOptions {
        mode: OutputMode::None,
        event_detail: EventDetail::Full,
        cursor: session.latest_cursor(),
        max_output_bytes: 1,
        tail_lines: 1,
    });
    if let Some(object) = payload.as_object_mut() {
        object.insert("bytes_written".into(), json!(bytes_written));
        object.insert("stdin_closed".into(), json!(close_stdin));
        object.insert(
            "suggestion".into(),
            json!("输入已发送；使用 wait_command 获取后续输出"),
        );
    }
    Ok(tool_ok(payload))
}

pub(super) fn run_kill_session(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(run_kill_session_async(store, args))
}

pub(super) async fn run_kill_session_async(
    store: &SessionStore,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let session_id = required_session_id(args)?;
    let session = store.get(session_id)?;
    session.touch_attachment();
    let options = OutputOptions::from_args(args, OutputMode::Tail);
    let wait_ms = args
        .get("wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(5000)
        .min(30_000);
    let signal = args.get("signal").and_then(Value::as_str).unwrap_or("TERM");
    let wait_deadline = Instant::now() + Duration::from_millis(wait_ms);

    let running = session.is_running().await;
    let started_running = running;
    let mut killed = false;
    let mut status = "exited";
    let mut evicted = !started_running;

    if running {
        session.mark_termination_reason("killed");
        if session.exit_waiter_started.load(Ordering::Acquire) {
            if let Some(pid) = session.process_id {
                send_session_signal(pid, signal).await;
            }
            let _ =
                tokio::time::timeout(Duration::from_millis(wait_ms), session.wait_until_exited())
                    .await;
        } else {
            session.kill_and_wait().await;
        }
        if session.is_running().await {
            status = "terminating";
            evicted = false;
        } else {
            killed = true;
        }
    }

    if !session.is_running().await && !session.is_finalized() && wait_ms > 0 {
        let remaining = wait_deadline.saturating_duration_since(Instant::now());
        if !remaining.is_zero() {
            let _ = session
                .wait_for_change(session.latest_cursor(), remaining, "finalized")
                .await;
        }
    }
    if !session.is_running().await {
        if session.is_finalized() {
            status = if killed { "killed" } else { "exited" };
            evicted = !started_running;
        } else {
            status = "verifying";
            evicted = false;
        }
    }

    let mut payload = session.snapshot_with_options(options);
    if let Some(object) = payload.as_object_mut() {
        object.insert("killed".into(), json!(killed));
        object.insert("status".into(), json!(status));
        object.insert("evicted".into(), json!(evicted));
        if status == "terminating" {
            object.insert(
                "warnings".into(),
                json!(["Process did not exit after kill; session retained for retry"]),
            );
        }
        if status == "verifying" {
            object.insert(
                "warnings".into(),
                json!(["Process exited; sandbox cleanup or verification is still pending"]),
            );
            object.insert(
                "suggestion".into(),
                json!("继续使用 wait_command 並指定 until=finalized，等待 sandbox cleanup / verification 完成"),
            );
        }
    }

    if evicted {
        store.remove(session_id);
    }

    Ok(tool_ok(payload))
}

pub(super) fn required_session_id(args: &Value) -> Result<&str, WorkspaceError> {
    let session_id = args
        .get("session_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("session_id is required"))?;
    if let Some(rest) = session_id.strip_prefix("output://") {
        let corrected = rest.rsplit_once('/').map(|(id, _)| id).unwrap_or(rest);
        return Err(WorkspaceError::ToolDetails {
            code: "OUTPUT_REF_USED_AS_SESSION_ID",
            message: "An output_ref was supplied where a session_id is required.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "received": session_id,
                "corrected_session_id": corrected,
                "suggestion": "Use the top-level session_id for wait_command, send_input, or kill_session; use output_ref only with read_output."
            }),
        });
    }
    Ok(session_id)
}

#[cfg(unix)]
async fn send_session_signal(pid: u32, signal: &str) {
    let sig = match signal {
        "KILL" => libc::SIGKILL,
        "INT" => libc::SIGINT,
        _ => libc::SIGTERM,
    };
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

#[cfg(windows)]
async fn send_session_signal(pid: u32, _signal: &str) {
    terminate_process(pid, true).await;
}
