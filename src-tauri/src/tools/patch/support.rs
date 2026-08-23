use std::fs;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use similar::TextDiff;

use crate::tools::workspace::{Workspace, WorkspaceError};

pub(super) fn unified_diff(
    path: &str,
    original: &str,
    updated: &str,
    is_new_file: bool,
    is_deleted: bool,
) -> String {
    let old_header = if is_new_file {
        "/dev/null".to_string()
    } else {
        format!("a/{path}")
    };
    let new_header = if is_deleted {
        "/dev/null".to_string()
    } else {
        format!("b/{path}")
    };
    TextDiff::from_lines(original, updated)
        .unified_diff()
        .context_radius(3)
        .header(&old_header, &new_header)
        .to_string()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            serde_json::to_string(value).expect("serializing canonical JSON scalar")
        }
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let fields = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).expect("serializing canonical JSON key"),
                        canonical_json(&object[key])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{fields}}}")
        }
    }
}

pub(super) fn replayable_edit_plan(
    tool: &str,
    arguments: Value,
    files: Value,
    stateful_dependencies: Value,
) -> Value {
    let mut plan = json!({
        "schema_version": 1,
        "tool": tool,
        "arguments": arguments,
        "expected_result": { "files": files },
        "stateful_dependencies": stateful_dependencies
    });
    let digest = sha256_hex(canonical_json(&plan).as_bytes());
    plan["plan_sha256"] = json!(digest);
    plan
}

pub(super) fn replay_reason(args: &Value) -> Option<Value> {
    args.get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .map(|reason| json!(reason))
}

pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

pub(super) fn version_mismatch(path: &str, expected: &str, actual: &str) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code: "FILE_VERSION_MISMATCH",
        message: format!("File changed since it was read: {path}"),
        category: "conflict",
        retryable: true,
        details: json!({
            "path": path,
            "expected_sha256": expected,
            "actual_sha256": actual,
            "suggestion": "Read the file again and rebuild the edit or patch",
            "recovery_actions": edit_recovery_actions(path, actual, "file_version_changed")
        }),
    }
}

pub(super) fn enrich_edit_error(
    error: WorkspaceError,
    path: &str,
    actual_sha256: &str,
) -> WorkspaceError {
    match error {
        WorkspaceError::ToolDetails {
            code,
            message,
            category,
            retryable,
            mut details,
        } => {
            if let Some(object) = details.as_object_mut() {
                object
                    .entry("path".to_string())
                    .or_insert_with(|| json!(path));
                object
                    .entry("actual_sha256".to_string())
                    .or_insert_with(|| json!(actual_sha256));
                object
                    .entry("recovery_actions".to_string())
                    .or_insert_with(|| json!(edit_recovery_actions(path, actual_sha256, code)));
            }
            WorkspaceError::ToolDetails {
                code,
                message,
                category,
                retryable,
                details,
            }
        }
        other => other,
    }
}

pub(super) fn enrich_edit_many_error(
    error: WorkspaceError,
    path: &str,
    actual_sha256: &str,
    file_index: usize,
) -> WorkspaceError {
    match enrich_edit_error(error, path, actual_sha256) {
        WorkspaceError::ToolDetails {
            code,
            message,
            category,
            retryable,
            mut details,
        } => {
            if let Some(object) = details.as_object_mut() {
                object
                    .entry("file_index".to_string())
                    .or_insert_with(|| json!(file_index));
            }
            WorkspaceError::ToolDetails {
                code,
                message,
                category,
                retryable,
                details,
            }
        }
        other => other,
    }
}

fn edit_recovery_actions(path: &str, actual_sha256: &str, reason: &str) -> Vec<Value> {
    vec![
        json!({
            "action": "read_current_file",
            "tool": "read_file",
            "arguments": { "path": path },
            "reason": reason
        }),
        json!({
            "action": "rebuild_guarded_edit",
            "tool": "edit",
            "arguments": {
                "files": [{
                    "path": path,
                    "expected_sha256": actual_sha256
                }]
            },
            "required_arguments": ["files[0].edits"],
            "reason": "rebuild_from_fresh_content"
        }),
    ]
}

pub(super) fn expected_hash_for<'a>(args: &'a Value, path: &str) -> Option<&'a str> {
    let hashes = args.get("expected_sha256")?.as_object()?;
    hashes
        .get(path)
        .or_else(|| hashes.get(&path.replace('\\', "/")))
        .and_then(Value::as_str)
}

pub(super) fn verify_file_version(
    ws: &Workspace,
    path: &str,
    expected_sha256: Option<&str>,
) -> Result<(), WorkspaceError> {
    match expected_sha256 {
        Some(expected) => {
            let resolved = ws.resolve_existing(path)?;
            let actual = sha256_hex(
                &fs::read(&resolved.path)
                    .map_err(|_| WorkspaceError::not_found(format!("File not found: {path}")))?,
            );
            if !expected.eq_ignore_ascii_case(&actual) {
                return Err(version_mismatch(path, expected, &actual));
            }
        }
        None => {
            if let Ok(resolved) = ws.resolve_existing(path) {
                let actual_sha256 = fs::read(&resolved.path)
                    .ok()
                    .map(|bytes| sha256_hex(&bytes))
                    .unwrap_or_default();
                return Err(WorkspaceError::ToolDetails {
                    code: "FILE_VERSION_MISMATCH",
                    message: format!("New file target appeared during preflight: {path}"),
                    category: "conflict",
                    retryable: true,
                    details: json!({
                        "path": path,
                        "expected": "missing",
                        "actual": "exists",
                        "actual_sha256": actual_sha256,
                        "recovery_actions": edit_recovery_actions(
                            path,
                            &actual_sha256,
                            "new_file_target_appeared"
                        )
                    }),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn affected_paths(affected: &[Value], operation: &str) -> Vec<String> {
    affected
        .iter()
        .filter(|file| file["operation"] == operation)
        .filter_map(|file| file["path"].as_str().map(str::to_string))
        .collect()
}

pub(super) fn is_critical_file(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    if matches!(first, ".git" | ".github") {
        return true;
    }
    let name = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    name == ".gitignore"
        || name == "Cargo.toml"
        || name == "Cargo.lock"
        || name == "package.json"
        || name == "package-lock.json"
        || name == "pnpm-lock.yaml"
        || name == "tauri.conf.json"
        || name.starts_with("README")
        || name.starts_with("LICENSE")
        || name.starts_with("vite.config.")
        || name == "pyproject.toml"
}

pub(super) fn is_protected_repository_asset(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    matches!(first, ".git" | ".github")
}

pub(super) fn dangerous_operation(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        message: message.into(),
        category: "permission",
        retryable: false,
    }
}

pub(super) fn protected_repository_asset(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PROTECTED_REPOSITORY_ASSET",
        message: message.into(),
        category: "security",
        retryable: false,
    }
}

pub(super) fn patch_failed(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_FAILED",
        message: message.into(),
        category: "validation",
        retryable: false,
    }
}
