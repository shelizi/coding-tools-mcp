use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::mcp::runtime_boot_id;

use super::{ExecSession, FINALIZED_SESSION_RETENTION};

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn operation_id_hash(operation_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(operation_id.as_bytes());
    format!("{:x}", hasher.finalize())
}

pub(super) fn record_path(dir: &Path, operation_id: &str) -> PathBuf {
    dir.join(format!("{}.json", operation_id_hash(operation_id)))
}

pub(super) fn bind_record_path(session: &ExecSession, dir: Option<&Path>) {
    let Some(dir) = dir else {
        return;
    };
    let Some(operation_id) = session.operation_id.as_deref() else {
        return;
    };
    let managed_job = session
        .process_timeout
        .as_ref()
        .is_some_and(|contract| contract.execution_mode == "job");
    if !managed_job {
        return;
    }
    let path = record_path(dir, operation_id);
    *session
        .operation_record_path
        .lock()
        .expect("operation record path lock") = Some(path);
    persist_session(session);
}

pub(super) fn persist_session(session: &ExecSession) {
    let path = session
        .operation_record_path
        .lock()
        .expect("operation record path lock")
        .clone();
    let Some(path) = path else {
        return;
    };
    let Some(operation_id) = session.operation_id.as_deref() else {
        return;
    };

    let mut snapshot = session.summary();
    if let Some(object) = snapshot.as_object_mut() {
        object.remove("operation_id");
        object.remove("output_refs");
        object.insert("stdout".into(), Value::String(String::new()));
        object.insert("stderr".into(), Value::String(String::new()));
        object.insert("events".into(), json!([]));
    }
    let record = json!({
        "schema_version": 1,
        "operation_id_hash": operation_id_hash(operation_id),
        "runtime_boot_id": runtime_boot_id(),
        "session_id": session.session_id,
        "command_fingerprint": session.command_fingerprint,
        "started_ts_ms": session.started_ts_ms,
        "updated_ts_ms": now_ms(),
        "finalized": session.is_finalized(),
        "snapshot": snapshot
    });

    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let Ok(bytes) = serde_json::to_vec(&record) else {
        return;
    };
    if fs::write(&temp, bytes).is_err() {
        return;
    }
    if path.exists() {
        let _ = fs::remove_file(&path);
    }
    if fs::rename(&temp, &path).is_err() {
        let _ = fs::remove_file(&temp);
    }
}

pub(super) fn load_record(dir: Option<&Path>, operation_id: &str) -> Option<Value> {
    let dir = dir?;
    let path = record_path(dir, operation_id);
    let bytes = fs::read(&path).ok()?;
    let record: Value = serde_json::from_slice(&bytes).ok()?;
    let expected_hash = operation_id_hash(operation_id);
    if record.get("operation_id_hash").and_then(Value::as_str) != Some(expected_hash.as_str()) {
        return None;
    }
    let updated_ts_ms = record.get("updated_ts_ms").and_then(Value::as_u64)?;
    let retention_ms = FINALIZED_SESSION_RETENTION.as_millis() as u64;
    if now_ms().saturating_sub(updated_ts_ms) > retention_ms {
        let _ = fs::remove_file(path);
        return None;
    }
    Some(record)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_record_path_never_contains_plaintext_id() {
        let dir = Path::new("operations");
        let path = record_path(dir, "customer-build-secret-name");
        let text = path.to_string_lossy();
        assert!(!text.contains("customer-build-secret-name"));
        assert!(text.ends_with(".json"));
    }
}
