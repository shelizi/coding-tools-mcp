use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::tools::redaction::{contains_sensitive_path, redact_sensitive_text};

pub(crate) const ACTIVE_WINDOW_MS: u64 = 120_000;
const MAX_ACTION_CHARS: usize = 120;

static NEXT_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SESSION_STATES: OnceLock<Mutex<HashMap<(String, String), SessionActivityState>>> =
    OnceLock::new();

#[derive(Clone, Debug)]
struct ActiveRequest {
    tool: String,
    action: String,
}

#[derive(Default)]
struct SessionActivityState {
    last_activity_ts_ms: u64,
    last_tool: String,
    last_action: String,
    last_outcome: String,
    active_requests: BTreeMap<u64, ActiveRequest>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionActivitySnapshot {
    pub status: String,
    pub tool: String,
    pub action: String,
    pub last_activity_at_ms: u64,
    pub active_request_count: usize,
    pub last_outcome: String,
}

pub(crate) struct SessionActivityGuard {
    profile_id: String,
    session_key: String,
    request_sequence: u64,
    finished: bool,
}

impl SessionActivityGuard {
    pub(crate) fn complete(&mut self, outcome: &str, completed_ts_ms: u128) {
        if self.finished {
            return;
        }
        finish_request(
            &self.profile_id,
            &self.session_key,
            self.request_sequence,
            outcome,
            bounded_timestamp(completed_ts_ms),
        );
        self.finished = true;
    }
}

impl Drop for SessionActivityGuard {
    fn drop(&mut self) {
        if !self.finished {
            finish_request(
                &self.profile_id,
                &self.session_key,
                self.request_sequence,
                "cancelled",
                unix_timestamp_ms(),
            );
            self.finished = true;
        }
    }
}

pub(crate) fn begin(
    profile_id: &str,
    session_key: Option<&str>,
    tool_name: &str,
    arguments: &Value,
    started_ts_ms: u128,
) -> Option<SessionActivityGuard> {
    let session_key = session_key
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    if tool_name.trim().is_empty() {
        return None;
    }

    let request_sequence = NEXT_REQUEST_SEQUENCE
        .fetch_add(1, Ordering::Relaxed)
        .saturating_add(1);
    let started_ts_ms = bounded_timestamp(started_ts_ms);
    let action = action_summary(tool_name, arguments);
    let request = ActiveRequest {
        tool: tool_name.to_string(),
        action: action.clone(),
    };
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = states
        .entry((profile_id.to_string(), session_key.to_string()))
        .or_default();
    state.last_activity_ts_ms = state.last_activity_ts_ms.max(started_ts_ms);
    state.last_tool = tool_name.to_string();
    state.last_action = action;
    state.active_requests.insert(request_sequence, request);

    Some(SessionActivityGuard {
        profile_id: profile_id.to_string(),
        session_key: session_key.to_string(),
        request_sequence,
        finished: false,
    })
}

pub(crate) fn snapshot(
    profile_id: &str,
    session_key: &str,
    now_ts_ms: u64,
) -> Option<SessionActivitySnapshot> {
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = states.get(&(profile_id.to_string(), session_key.to_string()))?;
    let current = state
        .active_requests
        .last_key_value()
        .map(|(_, value)| value);
    let active_request_count = state.active_requests.len();
    let status = if active_request_count > 0 {
        "running"
    } else if now_ts_ms.saturating_sub(state.last_activity_ts_ms) <= ACTIVE_WINDOW_MS {
        "active"
    } else {
        "inactive"
    };
    Some(SessionActivitySnapshot {
        status: status.to_string(),
        tool: current
            .map(|request| request.tool.clone())
            .unwrap_or_else(|| state.last_tool.clone()),
        action: current
            .map(|request| request.action.clone())
            .unwrap_or_else(|| state.last_action.clone()),
        last_activity_at_ms: state.last_activity_ts_ms,
        active_request_count,
        last_outcome: state.last_outcome.clone(),
    })
}

pub(crate) fn now_ms() -> u64 {
    unix_timestamp_ms()
}

fn finish_request(
    profile_id: &str,
    session_key: &str,
    request_sequence: u64,
    outcome: &str,
    completed_ts_ms: u64,
) {
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = states.get_mut(&(profile_id.to_string(), session_key.to_string())) else {
        return;
    };
    if let Some(completed) = state.active_requests.remove(&request_sequence) {
        state.last_tool = completed.tool;
        state.last_action = completed.action;
    }
    state.last_activity_ts_ms = state.last_activity_ts_ms.max(completed_ts_ms);
    state.last_outcome = outcome.to_string();
}

fn action_summary(tool_name: &str, arguments: &Value) -> String {
    let detail = if tool_name == "exec_many" {
        arguments
            .get("commands")
            .and_then(Value::as_array)
            .map(|commands| format!("{} commands", commands.len()))
    } else {
        [
            "path",
            "query",
            "pattern",
            "folder_id",
            "cmd",
            "script",
            "program",
            "session_id",
            "output_ref",
            "workdir",
        ]
        .iter()
        .find_map(|field| arguments.get(*field).and_then(Value::as_str))
        .map(safe_detail)
    };
    match detail.filter(|value| !value.is_empty()) {
        Some(detail) => truncate_chars(&format!("{tool_name} · {detail}"), MAX_ACTION_CHARS),
        None => tool_name.to_string(),
    }
}

fn safe_detail(value: &str) -> String {
    if contains_sensitive_path(value) {
        return "[sensitive path]".to_string();
    }
    let (redacted, _) = redact_sensitive_text(value);
    truncate_chars(redacted.trim(), MAX_ACTION_CHARS)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}…")
    } else {
        truncated
    }
}

fn bounded_timestamp(value: u128) -> u64 {
    value.min(u64::MAX as u128) as u64
}

fn unix_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{begin, snapshot, ACTIVE_WINDOW_MS};

    #[test]
    fn reports_running_active_and_inactive_states() {
        let profile = "activity-state-profile";
        let session = "activity-state-session";
        let mut guard = begin(
            profile,
            Some(session),
            "read_file",
            &json!({"path": "src/main.rs"}),
            1_000,
        )
        .expect("activity guard");
        let running = snapshot(profile, session, 1_001).expect("running snapshot");
        assert_eq!(running.status, "running");
        assert_eq!(running.action, "read_file · src/main.rs");
        assert_eq!(running.active_request_count, 1);

        guard.complete("success", 2_000);
        assert_eq!(
            snapshot(profile, session, 2_001)
                .expect("active snapshot")
                .status,
            "active"
        );
        assert_eq!(
            snapshot(profile, session, 2_001 + ACTIVE_WINDOW_MS + 1)
                .expect("inactive snapshot")
                .status,
            "inactive"
        );
    }

    #[test]
    fn dropping_guard_clears_running_request() {
        let profile = "activity-drop-profile";
        let session = "activity-drop-session";
        let guard = begin(
            profile,
            Some(session),
            "search_text",
            &json!({"query": "todo"}),
            1,
        )
        .expect("activity guard");
        assert_eq!(
            snapshot(profile, session, 2)
                .expect("running snapshot")
                .active_request_count,
            1
        );
        drop(guard);
        let cleaned = snapshot(profile, session, u64::MAX).expect("cleaned snapshot");
        assert_eq!(cleaned.active_request_count, 0);
        assert_eq!(cleaned.last_outcome, "cancelled");
    }

    #[test]
    fn parallel_completion_keeps_latest_remaining_action() {
        let profile = "activity-parallel-profile";
        let session = "activity-parallel-session";
        let mut first = begin(
            profile,
            Some(session),
            "read_file",
            &json!({"path": "a.rs"}),
            10,
        )
        .expect("first guard");
        let mut second = begin(
            profile,
            Some(session),
            "exec_command",
            &json!({"cmd": "cargo test"}),
            11,
        )
        .expect("second guard");

        second.complete("success", 12);
        let remaining = snapshot(profile, session, 13).expect("remaining snapshot");
        assert_eq!(remaining.status, "running");
        assert_eq!(remaining.action, "read_file · a.rs");
        first.complete("success", 14);
    }

    #[test]
    fn ignores_missing_session_and_redacts_action_details() {
        assert!(begin("profile", None, "read_file", &json!({}), 1).is_none());
        let mut guard = begin(
            "redaction-profile",
            Some("redaction-session"),
            "exec_command",
            &json!({"cmd": "curl -H 'Authorization: Bearer hidden-token' https://example.com"}),
            1,
        )
        .expect("redacted activity guard");
        let action = snapshot("redaction-profile", "redaction-session", 2)
            .expect("redacted snapshot")
            .action;
        assert!(!action.contains("hidden-token"));
        assert!(action.contains("[REDACTED]"));
        guard.complete("success", 3);
    }
}
