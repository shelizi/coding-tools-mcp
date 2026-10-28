use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use super::session_activity;
use crate::tools::history::{persist_auto_activity, AutoActivityEvent};

const FLUSH_DEBOUNCE: Duration = Duration::from_secs(5);
const MAX_PENDING_SESSIONS: usize = 128;
const MAX_RETRIES: usize = 2;
// Background status is rechecked every ten seconds. Refresh its durable
// observation once a minute while it is truly running, otherwise a Node
// dashboard marks this Rust job idle after the shared five-minute window.
const BACKGROUND_HEARTBEAT_MS: u64 = 60_000;

fn background_observation_id(pid: u32, digest: &str, running: bool, at_ms: u64) -> String {
    if running {
        format!(
            "rust:{pid}:{digest}:{}:background_running",
            at_ms / BACKGROUND_HEARTBEAT_MS
        )
    } else {
        format!("rust:{pid}:{digest}:background_ended")
    }
}

/// Background jobs are polled every ten seconds, but only generate a new
/// durable heartbeat ID once per minute. Skip writes for repeated IDs.
fn activity_signature(root: &Path, events: &[AutoActivityEvent]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(root.to_string_lossy().as_bytes());
    let mut ordered = events.iter().collect::<Vec<_>>();
    ordered.sort_unstable_by(|left, right| left.id.cmp(&right.id));
    for event in ordered {
        hasher.update([0]);
        hasher.update(event.id.as_bytes());
        hasher.update([0]);
        hasher.update(event.status.as_bytes());
    }
    hasher.finalize().into()
}

/// Coalesce start/finish tool notifications by (profile, native conversation).
/// A single worker reads the latest bounded tool state and writes under the
/// shared history archive lock after a short delay. Never block MCP replies.
static PENDING: OnceLock<Mutex<HashMap<(String, String), u64>>> = OnceLock::new();

pub(crate) fn schedule(profile: &str, session_key: Option<&str>) {
    let Some(session_key) = session_key.filter(|key| !key.trim().is_empty()) else {
        return;
    };
    let pending = PENDING.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (profile.to_owned(), session_key.to_owned());
    let mut jobs = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(version) = jobs.get_mut(&key) {
        *version = version.saturating_add(1);
        return;
    }
    if jobs.len() >= MAX_PENDING_SESSIONS {
        return;
    }
    jobs.insert(key.clone(), 1);
    drop(jobs);

    let cleanup_key = key.clone();
    let spawned = std::thread::Builder::new()
        .name("ctmcp-history-progress".into())
        .spawn(move || {
            let mut retries = 0;
            let mut wait = FLUSH_DEBOUNCE;
            let mut last_persisted = None;
            loop {
                std::thread::sleep(wait);
                let seen = {
                    let pending = PENDING.get().expect("initialized");
                    let jobs = pending
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    let Some(version) = jobs.get(&key) else { break };
                    *version
                };
                let written = flush(&key.0, &key.1, &mut last_persisted);
                let background_running = matches!(written, Ok(true));
                let write_succeeded = written.is_ok();
                wait = if background_running {
                    Duration::from_secs(10)
                } else {
                    FLUSH_DEBOUNCE
                };
                if written.is_err() {
                    retries += 1
                } else {
                    retries = 0
                }
                let pending = PENDING.get().expect("initialized");
                let mut jobs = pending
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if retries > MAX_RETRIES
                    || (write_succeeded
                        && !background_running
                        && jobs.get(&key).is_some_and(|version| *version == seen))
                {
                    jobs.remove(&key);
                    break;
                }
            }
        });
    if spawned.is_err() {
        pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&cleanup_key);
    }
}

fn flush(
    profile: &str,
    session_key: &str,
    last_persisted: &mut Option<[u8; 32]>,
) -> crate::tools::workspace::WorkspaceResult<bool> {
    let Some(root) = crate::tools::hub::history_root_for_host_session(profile, session_key) else {
        return Ok(false);
    };
    let mut events = session_activity::recent_actions(profile, session_key)
        .into_iter()
        .map(|event| AutoActivityEvent {
            id: format!(
                "rust:{}:{}:{}:{}",
                std::process::id(),
                event.started_at_ms,
                event.sequence,
                event.status
            ),
            at_ms: event.updated_at_ms,
            tool: event.tool,
            action: event.action,
            status: event.status,
        })
        .collect::<Vec<_>>();
    let mut any_running = false;
    for reference in session_activity::background_job_refs(profile, session_key) {
        let Some((running, completed_at)) =
            crate::tools::hub::session_process_state(profile, &reference.id)
        else {
            continue;
        };
        any_running |= running;
        let suffix = if running {
            "background_running"
        } else {
            "background_ended"
        };
        let digest = format!("{:x}", Sha256::digest(reference.id.as_bytes()));
        let observed_at_ms = session_activity::now_ms();
        events.push(AutoActivityEvent {
            id: background_observation_id(
                std::process::id(),
                &digest[..16],
                running,
                observed_at_ms,
            ),
            at_ms: if running {
                observed_at_ms
            } else {
                completed_at.unwrap_or(observed_at_ms)
            },
            tool: reference.tool,
            action: reference.action,
            status: suffix.to_string(),
        });
    }
    let signature = activity_signature(&root, &events);
    if last_persisted.as_ref() != Some(&signature) {
        // Do not cache failures or missing history-session mappings.
        if persist_auto_activity(&root, session_key, &events)? {
            *last_persisted = Some(signature);
        }
    }
    Ok(any_running)
}

#[cfg(test)]
mod tests {
    use super::{
        activity_signature, background_observation_id, AutoActivityEvent, BACKGROUND_HEARTBEAT_MS,
    };
    use std::path::Path;

    #[test]
    fn unchanged_heartbeat_does_not_require_another_history_write() {
        let event = AutoActivityEvent {
            id: background_observation_id(42, "job", true, 60_000),
            at_ms: 60_000,
            tool: "exec_command".into(),
            action: "run".into(),
            status: "background_running".into(),
        };
        let root = Path::new("workspace-one");
        let first = activity_signature(root, &[event.clone()]);
        let mut polled_again = event.clone();
        polled_again.at_ms += 10_000;
        assert_eq!(first, activity_signature(root, &[polled_again]));
        assert_ne!(
            first,
            activity_signature(Path::new("workspace-two"), &[event.clone()])
        );
        let next_minute = AutoActivityEvent {
            id: background_observation_id(42, "job", true, 120_000),
            ..event.clone()
        };
        assert_ne!(first, activity_signature(root, &[next_minute]));
        assert_ne!(first, activity_signature(root, &[event.clone(), event]));
        let alpha = AutoActivityEvent {
            id: "alpha".into(),
            at_ms: 1,
            tool: "exec_command".into(),
            action: "run".into(),
            status: "success".into(),
        };
        let beta = AutoActivityEvent {
            id: "beta".into(),
            ..alpha.clone()
        };
        assert_eq!(
            activity_signature(root, &[alpha.clone(), beta.clone()]),
            activity_signature(root, &[beta, alpha])
        );
    }

    #[test]
    fn running_background_heartbeat_advances_only_once_per_minute() {
        let prefix = "abcdef0123456789";
        let minute = BACKGROUND_HEARTBEAT_MS * 100;
        let first = background_observation_id(42, prefix, true, minute);
        assert_eq!(
            first,
            background_observation_id(42, prefix, true, minute + 50_000)
        );
        assert_ne!(
            first,
            background_observation_id(42, prefix, true, minute + 60_000)
        );
        assert!(first.ends_with(":background_running"));
        assert_eq!(
            background_observation_id(42, prefix, false, minute + 600_000),
            background_observation_id(42, prefix, false, minute + 1_200_000)
        );
    }
}
