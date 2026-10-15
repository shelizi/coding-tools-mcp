use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::time::Instant;

use serde_json::Value;

use crate::mcp::{record_async_session_finalized, AsyncSessionTelemetry};
use crate::tools::dispatch::operation_result_summary;

use super::operation_persistence;
use super::{
    ExecSession, SessionRegistry, FINALIZED_SESSION_RETENTION, MAX_RETAINED_FINALIZED_SESSIONS,
};

const AUDIT_OUTPUT_TAIL_BYTES: usize = 16 * 1024;

fn audit_output_tail(value: &str) -> (String, usize, bool) {
    let bytes = value.as_bytes();
    if bytes.len() <= AUDIT_OUTPUT_TAIL_BYTES {
        return (value.to_string(), bytes.len(), false);
    }
    let mut start = bytes.len() - AUDIT_OUTPUT_TAIL_BYTES;
    while start < bytes.len() && !value.is_char_boundary(start) {
        start += 1;
    }
    (value[start..].to_string(), bytes.len() - start, true)
}

pub(super) fn prune_finalized_sessions(registry: &mut SessionRegistry) {
    registry.sessions.retain(|_, session| {
        !session
            .finalized_at()
            .is_some_and(|finished| finished.elapsed() >= FINALIZED_SESSION_RETENTION)
    });

    let mut finalized = registry
        .sessions
        .iter()
        .filter_map(|(session_id, session)| {
            session
                .finalized_at()
                .map(|finished| (session_id.clone(), finished))
        })
        .collect::<Vec<_>>();
    if finalized.len() > MAX_RETAINED_FINALIZED_SESSIONS {
        finalized.sort_by_key(|(_, finished)| *finished);
        let remove_count = finalized.len() - MAX_RETAINED_FINALIZED_SESSIONS;
        for (session_id, _) in finalized.into_iter().take(remove_count) {
            registry.sessions.remove(&session_id);
        }
    }
    let retained_session_ids = registry.sessions.keys().cloned().collect::<HashSet<_>>();
    registry
        .operation_index
        .retain(|_, session_id| retained_session_ids.contains(session_id));
    registry
        .fingerprint_index
        .retain(|_, session_id| retained_session_ids.contains(session_id));
}

pub(super) fn finish_session(session: &ExecSession) {
    if session.finalized.swap(true, Ordering::AcqRel) {
        return;
    }
    let mut finalized_at = session.finalized_at.lock().expect("finalized_at lock");
    if finalized_at.is_none() {
        *finalized_at = Some(Instant::now());
    }
    drop(finalized_at);
    session.active_slot.lock().expect("active slot lock").take();
    operation_persistence::persist_session(session);
    record_finalization_telemetry(session);
    record_harness_operation_finalization(session);
    session.notify_change();
}

pub(super) fn record_harness_operation_finalization(session: &ExecSession) {
    let operations = session
        .harness_operations
        .lock()
        .expect("harness operation lock")
        .clone();
    for tracking in operations {
        {
            let mut recorded = session
                .harness_operation_recorded
                .lock()
                .expect("harness operation recorded lock");
            if !recorded.insert(tracking.operation.id.clone()) {
                continue;
            }
        }
        let summary = operation_result_summary(&tracking.operation.tool, &session.summary());
        let kind = if summary.get("command_ok").and_then(Value::as_bool) == Some(true) {
            "completed"
        } else {
            "failed"
        };
        if tracking
            .harness
            .record_operation(
                Some(&tracking.operation.id),
                tracking.operation.task_id.as_deref(),
                &tracking.operation.tool,
                kind,
                tracking.operation.input_summary.clone(),
                summary,
            )
            .is_err()
        {
            session
                .harness_operation_recorded
                .lock()
                .expect("harness operation recorded lock")
                .remove(&tracking.operation.id);
        }
    }
}

fn record_finalization_telemetry(session: &ExecSession) {
    let Some(profile_id) = session.telemetry_profile_id.as_deref() else {
        return;
    };
    let first_output_ms = session
        .first_output_at
        .lock()
        .expect("first output lock")
        .map(|instant| {
            instant
                .saturating_duration_since(session.started_at)
                .as_millis() as u64
        });
    let termination_reason = session
        .termination_reason
        .lock()
        .expect("termination lock")
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let stdout = session.stdout.lock().expect("stdout lock").snapshot();
    let stderr = session.stderr.lock().expect("stderr lock").snapshot();
    let sensitive = session.has_sensitive_output();
    let stdout_text = if sensitive {
        "[REDACTED]".to_string()
    } else {
        super::output::decode_process_output_with_encoding(&stdout.data, stdout.encoding)
    };
    let stderr_text = if sensitive {
        "[REDACTED]".to_string()
    } else {
        super::output::decode_process_output_with_encoding(&stderr.data, stderr.encoding)
    };
    let (stdout_audit, stdout_audit_tail_bytes, stdout_tail_truncated) =
        audit_output_tail(&stdout_text);
    let (stderr_audit, stderr_audit_tail_bytes, stderr_tail_truncated) =
        audit_output_tail(&stderr_text);
    record_async_session_finalized(AsyncSessionTelemetry {
        profile_id,
        session_id: &session.session_id,
        command_kind: &session.telemetry_command_kind,
        started_ts_ms: session.started_ts_ms,
        child_process_total_ms: session.started_at.elapsed().as_millis() as u64,
        first_output_ms,
        exit_code: *session.exit_code.lock().expect("exit_code lock"),
        termination_reason: &termination_reason,
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout: &stdout_audit,
        stderr: &stderr_audit,
        stdout_audit_tail_bytes,
        stderr_audit_tail_bytes,
        stdout_audit_tail_truncated: stdout_tail_truncated
            || stdout.total_bytes > stdout.data.len(),
        stderr_audit_tail_truncated: stderr_tail_truncated
            || stderr.total_bytes > stderr.data.len(),
    });
}
