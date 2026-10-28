use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::tools::redaction::{contains_sensitive_path, redact_sensitive_text};

// Live requests and managed processes are the only "running" signals.
// Recently active work remains visible between intermittent tool calls.
pub(crate) const ACTIVE_WINDOW_MS: u64 = 5 * 60_000;
const MAX_ACTION_CHARS: usize = 120;
const MAX_RECENT_ACTIONS: usize = 16;

static NEXT_REQUEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static SESSION_STATES: OnceLock<Mutex<HashMap<(String, String), SessionActivityState>>> =
    OnceLock::new();
static WORKSPACE_LAST_ACCESS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

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
    background_jobs: HashMap<String, ActiveRequest>,
    recent_actions: VecDeque<RecentAction>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RecentAction {
    pub(crate) sequence: u64,
    pub(crate) tool: String,
    pub(crate) action: String,
    pub(crate) started_at_ms: u64,
    pub(crate) updated_at_ms: u64,
    pub(crate) status: String,
}

pub(crate) fn recent_actions(profile_id: &str, session_key: &str) -> Vec<RecentAction> {
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = states.get(&(profile_id.to_owned(), session_key.to_owned())) else {
        return Vec::new();
    };
    let mut actions = state.recent_actions.iter().cloned().collect::<Vec<_>>();
    actions.sort_by_key(|item| {
        (
            std::cmp::Reverse(item.updated_at_ms),
            std::cmp::Reverse(item.sequence),
        )
    });
    actions
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

/// Internal process association; never return the archived conversation key to the UI.
#[derive(Clone, Debug)]
pub(crate) struct BackgroundJobRef {
    pub id: String,
    pub tool: String,
    pub action: String,
}

pub(crate) fn background_job_refs(profile_id: &str, session_key: &str) -> Vec<BackgroundJobRef> {
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    states
        .get(&(profile_id.to_owned(), session_key.to_owned()))
        .map(|state| {
            state
                .background_jobs
                .iter()
                .take(64)
                .map(|(id, request)| BackgroundJobRef {
                    id: id.clone(),
                    tool: request.tool.clone(),
                    action: request.action.clone(),
                })
                .collect()
        })
        .unwrap_or_default()
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
    state.recent_actions.push_back(RecentAction {
        sequence: request_sequence,
        tool: tool_name.to_owned(),
        action: state.last_action.clone(),
        started_at_ms: started_ts_ms,
        updated_at_ms: started_ts_ms,
        status: "running".into(),
    });
    if state.recent_actions.len() > MAX_RECENT_ACTIONS {
        state.recent_actions.pop_front();
    }

    Some(SessionActivityGuard {
        profile_id: profile_id.to_string(),
        session_key: session_key.to_string(),
        request_sequence,
        finished: false,
    })
}

pub(crate) fn observe_background_processes(
    profile_id: &str,
    session_key: Option<&str>,
    tool_name: &str,
    response: &Value,
) {
    if !matches!(
        tool_name,
        "exec_command" | "exec_many" | "exec_health_check"
    ) {
        return;
    }
    let Some(session_key) = session_key.filter(|key| !key.trim().is_empty()) else {
        return;
    };
    let Some(structured) = response
        .get("result")
        .and_then(|result| result.get("structuredContent"))
    else {
        return;
    };
    let mut candidates = vec![structured];
    if tool_name == "exec_many" {
        if let Some(results) = structured.get("results").and_then(Value::as_array) {
            for item in results.iter().take(256) {
                candidates.push(item.get("result").unwrap_or(item));
            }
        }
    }
    let ids = candidates
        .into_iter()
        .filter(|item| {
            matches!(
                item.get("status").and_then(Value::as_str),
                Some("running" | "verifying")
            )
        })
        .filter_map(|item| item.get("session_id").and_then(Value::as_str))
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return;
    }
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(state) = states.get_mut(&(profile_id.to_string(), session_key.to_string())) {
        for id in ids {
            if state.background_jobs.len() >= 64 && !state.background_jobs.contains_key(id) {
                if let Some(oldest) = state.background_jobs.keys().next().cloned() {
                    state.background_jobs.remove(&oldest);
                }
            }
            state.background_jobs.insert(
                id.to_string(),
                ActiveRequest {
                    tool: tool_name.to_string(),
                    action: state.last_action.clone(),
                },
            );
        }
    }
}

pub(crate) fn snapshot(
    profile_id: &str,
    session_key: &str,
    now_ts_ms: u64,
) -> Option<SessionActivitySnapshot> {
    snapshot_with_lookup(profile_id, session_key, now_ts_ms, |job_id| {
        crate::tools::hub::session_process_state(profile_id, job_id)
    })
}

fn snapshot_with_lookup(
    profile_id: &str,
    session_key: &str,
    now_ts_ms: u64,
    process_state: impl Fn(&str) -> Option<(bool, Option<u64>)>,
) -> Option<SessionActivitySnapshot> {
    let states = SESSION_STATES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = states.get_mut(&(profile_id.to_string(), session_key.to_string()))?;
    let mut running_job = None;
    let mut running_jobs = 0usize;
    state.background_jobs.retain(|id, request| {
        let (running, completed_at) = process_state(id).unwrap_or((false, None));
        if running {
            running_job = Some(request.clone());
            running_jobs += 1;
            true
        } else {
            if let Some(completed_at) = completed_at {
                state.last_activity_ts_ms = state.last_activity_ts_ms.max(completed_at);
            }
            false
        }
    });
    let current = state
        .active_requests
        .last_key_value()
        .map(|(_, value)| value)
        .or(running_job.as_ref());
    let active_request_count = state.active_requests.len() + running_jobs;
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

/// Record an authenticated MCP request for a workspace regardless of session
/// key or RPC method. Used by the overview to show the last access time.
pub(crate) fn record_workspace_access(profile_id: &str, ts_ms: u128) {
    let profile_id = profile_id.trim();
    let ts_ms = bounded_timestamp(ts_ms);
    if profile_id.is_empty() || ts_ms == 0 {
        return;
    }
    let states = WORKSPACE_LAST_ACCESS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = states.entry(profile_id.to_string()).or_insert(0);
    *entry = (*entry).max(ts_ms);
}

pub(crate) fn workspace_last_access_ms(profile_id: &str) -> Option<u64> {
    let states = WORKSPACE_LAST_ACCESS.get_or_init(|| Mutex::new(HashMap::new()));
    let states = states
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    states.get(profile_id).copied().filter(|ts| *ts > 0)
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
    if let Some(action) = state
        .recent_actions
        .iter_mut()
        .find(|entry| entry.sequence == request_sequence)
    {
        action.status = outcome.to_owned();
        action.updated_at_ms = completed_ts_ms;
    }
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

    use super::{
        background_job_refs, begin, observe_background_processes, recent_actions,
        record_workspace_access, snapshot, snapshot_with_lookup, workspace_last_access_ms,
        ACTIVE_WINDOW_MS,
    };

    #[test]
    fn recent_tool_activity_is_live_sanitized_scoped_and_bounded() {
        let profile = "recent-actions-profile";
        let key = "recent-actions-owner";
        let mut first = begin(
            profile,
            Some(key),
            "exec_command",
            &json!({"path":".ssh/id_rsa", "program":"cargo"}),
            1_000,
        )
        .expect("guard");
        let running = recent_actions(profile, key);
        assert_eq!(running[0].status, "running");
        assert!(!running[0].action.contains("id_rsa"));
        first.complete("success", 1_200);
        assert_eq!(recent_actions(profile, key)[0].status, "success");
        let mut later = begin(
            profile,
            Some(key),
            "read_file",
            &json!({"path":"src/lib.rs"}),
            1_300,
        )
        .expect("guard");
        later.complete("failed", 1_320);
        let latest = recent_actions(profile, key);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].tool, "read_file");
        assert_eq!(latest[0].status, "failed");
        assert_eq!(latest[0].updated_at_ms, 1_320);
        assert!(recent_actions(profile, "other-session").is_empty());
        assert!(recent_actions("other-profile", key).is_empty());
        for index in 0..25 {
            let mut guard = begin(
                profile,
                Some(key),
                "search_text",
                &json!({"query":"test"}),
                2_000 + index,
            )
            .expect("bounded guard");
            guard.complete("success", 2_500 + index as u128);
        }
        assert_eq!(recent_actions(profile, key).len(), 16);
    }

    #[test]
    fn history_job_associations_are_scoped_to_the_exact_archived_conversation() {
        let profile = "history-jobs-profile";
        let mut guard = begin(
            profile,
            Some("owner-one"),
            "exec_command",
            &json!({"program":"cargo"}),
            100,
        )
        .expect("guard");
        guard.complete("success", 150);
        observe_background_processes(
            profile,
            Some("owner-one"),
            "exec_command",
            &json!({"result":{"structuredContent":{"status":"running","session_id":"private-job"}}}),
        );
        let refs = background_job_refs(profile, "owner-one");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].id, "private-job");
        assert_eq!(refs[0].tool, "exec_command");
        assert!(background_job_refs(profile, "owner-two").is_empty());
        assert!(background_job_refs("other-profile", "owner-one").is_empty());
    }

    #[test]
    fn managed_background_command_remains_running_after_rpc_finishes_until_process_exits() {
        let profile = "background-running-profile";
        let session = "background-running-session";
        let mut guard = begin(
            profile,
            Some(session),
            "exec_command",
            &json!({"program":"cargo"}),
            1_000,
        )
        .expect("request guard");
        guard.complete("success", 1_010);
        observe_background_processes(
            profile,
            Some(session),
            "exec_command",
            &json!({"result":{"structuredContent":{"status":"running","session_id":"process-1"}}}),
        );
        let running = snapshot_with_lookup(profile, session, 1_010 + ACTIVE_WINDOW_MS * 4, |id| {
            assert_eq!(id, "process-1");
            Some((true, None))
        })
        .expect("running background");
        assert_eq!(running.status, "running");
        assert_eq!(running.active_request_count, 1);
        assert_eq!(running.tool, "exec_command");
        let finished_at = 1_010 + ACTIVE_WINDOW_MS * 4 + 20;
        let done = snapshot_with_lookup(profile, session, finished_at, |_| {
            Some((false, Some(finished_at)))
        })
        .expect("background finished");
        assert_eq!(done.status, "active");
        assert_eq!(done.active_request_count, 0);
        assert_eq!(done.last_activity_at_ms, finished_at);
        let inactive =
            snapshot_with_lookup(profile, session, finished_at + ACTIVE_WINDOW_MS + 1, |_| {
                panic!("completed process must be pruned")
            })
            .expect("inactive after window");
        assert_eq!(inactive.status, "inactive");
    }

    #[test]
    fn external_rpc_cannot_spoof_process_activity_and_batch_jobs_count_correctly() {
        let profile = "background-batch-profile";
        let session = "background-batch-session";
        let mut guard = begin(
            profile,
            Some(session),
            "exec_many",
            &json!({"commands":[{},{}]}),
            1_000,
        )
        .expect("guard");
        guard.complete("success", 1_020);
        observe_background_processes(
            profile,
            Some(session),
            "external_mcp",
            &json!({"result":{"structuredContent":{"status":"running","session_id":"fake"}}}),
        );
        let initial =
            snapshot_with_lookup(profile, session, 1_030, |_| Some((true, None))).expect("active");
        assert_eq!(initial.status, "active");
        observe_background_processes(
            profile,
            Some(session),
            "exec_many",
            &json!({
                "result":{"structuredContent":{"results":[
                    {"id":"a","result":{"status":"running","session_id":"batch-1"}},
                    {"id":"b","result":{"status":"verifying","session_id":"batch-2"}}
                ]}}
            }),
        );
        let running = snapshot_with_lookup(profile, session, 1_030, |_| Some((true, None)))
            .expect("multiple jobs running");
        assert_eq!(running.active_request_count, 2);
        let remaining =
            snapshot_with_lookup(profile, session, 1_040, |id| Some((id == "batch-2", None)))
                .expect("one job running");
        assert_eq!(remaining.active_request_count, 1);
        assert_eq!(
            snapshot_with_lookup(profile, session, 1_050, |_| Some((false, None)))
                .expect("all finished")
                .status,
            "active"
        );
    }

    #[test]
    fn workspace_access_tracks_latest_timestamp() {
        let profile = "workspace-access-profile";
        assert_eq!(workspace_last_access_ms(profile), None);
        record_workspace_access(profile, 1_000);
        record_workspace_access(profile, 500);
        record_workspace_access(profile, 2_000);
        assert_eq!(workspace_last_access_ms(profile), Some(2_000));
        record_workspace_access(" ", 9_999);
        record_workspace_access(profile, 0);
        assert_eq!(workspace_last_access_ms(profile), Some(2_000));
    }

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
