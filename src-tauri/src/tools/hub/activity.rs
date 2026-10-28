//! Runtime-local MCP workspace activity. Activity never comes from saved
//! conversation selections; it records actual tool calls and expires by age.
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use super::{lock_hubs, lock_state, RoutingState};
use crate::tools::SharedToolContext;
use crate::workspace::WorkspaceFolder;

const RETENTION_MS: u64 = 60 * 60_000;
const MAX_RECORDS: usize = 1024;

#[derive(Clone)]
pub(super) struct WorkspaceAccess {
    pub conversation_key: String,
    pub folder_id: String,
    pub checkout_path: String,
    pub kind: &'static str,
    pub last_access_ms: u64,
    pub tool_calls: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn checkout_for(folder: &WorkspaceFolder, name: &str, args: &Value) -> (String, &'static str) {
    let root = PathBuf::from(&folder.path);
    let raw = if name.starts_with("git_") {
        args.get("repo_path").or_else(|| args.get("path"))
    } else if name == "exec_command" {
        args.get("workdir")
    } else {
        args.get("path").or_else(|| args.get("workdir"))
    };
    let relative = raw
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or(".");
    let root = root.canonicalize().unwrap_or(root);
    let candidate = root.join(relative);
    let mut cursor = candidate.canonicalize().unwrap_or_else(|_| root.clone());
    if !cursor.starts_with(&root) {
        cursor = root.clone();
    }
    if !cursor.is_dir() {
        cursor = cursor.parent().unwrap_or(&root).to_path_buf();
    }
    loop {
        if !cursor.starts_with(&root) {
            break;
        }
        let marker = cursor.join(".git");
        if marker.is_file() {
            let gitdir = std::fs::read_to_string(&marker)
                .unwrap_or_default()
                .replace('\\', "/");
            return (
                cursor.to_string_lossy().into_owned(),
                if gitdir.contains("/modules/") {
                    "repository"
                } else {
                    "worktree"
                },
            );
        }
        if marker.is_dir() {
            return (cursor.to_string_lossy().into_owned(), "repository");
        }
        if cursor == root {
            break;
        }
        cursor = cursor.parent().unwrap_or(&root).to_path_buf();
    }
    (root.to_string_lossy().into_owned(), "folder")
}

fn prune(state: &mut RoutingState, now: u64) {
    state
        .workspace_access
        .retain(|_, record| now.saturating_sub(record.last_access_ms) <= RETENTION_MS);
    if state.workspace_access.len() <= MAX_RECORDS {
        return;
    }
    let mut oldest = state
        .workspace_access
        .iter()
        .map(|(key, value)| (key.clone(), value.last_access_ms))
        .collect::<Vec<_>>();
    oldest.sort_by_key(|(_, time)| *time);
    for (key, _) in oldest
        .into_iter()
        .take(state.workspace_access.len() - MAX_RECORDS)
    {
        state.workspace_access.remove(&key);
    }
}

pub fn record(
    fallback: &SharedToolContext,
    host_session_key: Option<&str>,
    folder_id: &str,
    name: &str,
    args: &Value,
) {
    if matches!(
        name,
        "list_workspace_folders"
            | "list_active_workspaces"
            | "conversation_bootstrap"
            | "switch_workspace_folder"
            | "server_info"
            | "query_tool_usage"
    ) {
        return;
    }
    let Some(conversation_key) = host_session_key.filter(|key| !key.is_empty()) else {
        return;
    };
    let Some(router) = lock_hubs().get(&fallback.profile_id).cloned() else {
        return;
    };
    let folder = {
        let state = lock_state(&router);
        state
            .folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .cloned()
    };
    let Some(folder) = folder else {
        return;
    };
    let (checkout_path, kind) = checkout_for(&folder, name, args);
    let now = now_ms();
    let mut state = lock_state(&router);
    let key = format!("{conversation_key}\0{folder_id}\0{checkout_path}");
    if let Some(record) = state.workspace_access.get_mut(&key) {
        record.last_access_ms = now;
        record.tool_calls += 1;
    } else {
        state.workspace_access.insert(
            key,
            WorkspaceAccess {
                conversation_key: conversation_key.to_string(),
                folder_id: folder_id.to_string(),
                checkout_path,
                kind,
                last_access_ms: now,
                tool_calls: 1,
            },
        );
    }
    prune(&mut state, now);
}

pub fn listing(fallback: &SharedToolContext, args: &Value) -> Value {
    let active_minutes = args
        .get("active_within_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(3);
    let recent_minutes = args
        .get("recent_within_minutes")
        .and_then(Value::as_u64)
        .unwrap_or(5);
    if !(1..=60).contains(&active_minutes)
        || !(active_minutes..=60).contains(&recent_minutes)
        || args
            .get("active_within_minutes")
            .is_some_and(|value| !value.is_u64())
        || args
            .get("recent_within_minutes")
            .is_some_and(|value| !value.is_u64())
    {
        return json!({
            "ok": false,
            "error": {
                "code": "INVALID_ARGUMENT",
                "category": "validation",
                "retryable": false,
                "message": "active_within_minutes and recent_within_minutes must be integers from 1 to 60, with recent >= active"
            }
        });
    }
    let Some(router) = lock_hubs().get(&fallback.profile_id).cloned() else {
        return json!({
            "ok": false,
            "error": {
                "code": "WORKSPACE_ROUTER_NOT_CONFIGURED",
                "category": "workspace_routing",
                "retryable": false,
                "message": "Tool hub routing is not initialized."
            }
        });
    };
    let now = now_ms();
    let active_cutoff = now.saturating_sub(active_minutes * 60_000);
    let recent_cutoff = now.saturating_sub(recent_minutes * 60_000);
    let mut state = lock_state(&router);
    prune(&mut state, now);
    let mut projects = Vec::new();
    let mut active_project_count = 0usize;
    let mut recent_project_count = 0usize;
    let mut active_worktree_count = 0usize;
    for folder in &state.folders {
        let entries = state
            .workspace_access
            .values()
            .filter(|entry| entry.folder_id == folder.id && entry.last_access_ms >= recent_cutoff)
            .collect::<Vec<_>>();
        if entries.is_empty() {
            continue;
        }
        let mut groups: HashMap<&str, Vec<&WorkspaceAccess>> = HashMap::new();
        for entry in &entries {
            groups
                .entry(entry.checkout_path.as_str())
                .or_default()
                .push(entry);
        }
        let mut checkouts = groups.values().map(|group| {
            let last = group.iter().map(|item| item.last_access_ms).max().unwrap_or(0);
            let status = if last >= active_cutoff { "active" } else { "recent" };
            if status == "active" && group[0].kind == "worktree" {
                active_worktree_count += 1;
            }
            json!({
                "path": group[0].checkout_path,
                "kind": group[0].kind,
                "status": status,
                "conversation_count": group.iter().map(|item| item.conversation_key.as_str()).collect::<HashSet<_>>().len(),
                "tool_call_count": group.iter().map(|item| item.tool_calls).sum::<u64>(),
                "last_access_at_ms": last
            })
        }).collect::<Vec<_>>();
        checkouts
            .sort_by_key(|item| std::cmp::Reverse(item["last_access_at_ms"].as_u64().unwrap_or(0)));
        let is_active = checkouts.iter().any(|item| item["status"] == "active");
        if is_active {
            active_project_count += 1;
        } else {
            recent_project_count += 1;
        }
        projects.push(json!({
            "folder_id": folder.id,
            "name": folder.name,
            "path": folder.path,
            "status": if is_active { "active" } else { "recent" },
            "conversation_count": entries.iter().map(|item| item.conversation_key.as_str()).collect::<HashSet<_>>().len(),
            "tool_call_count": entries.iter().map(|item| item.tool_calls).sum::<u64>(),
            "last_access_at_ms": entries.iter().map(|item| item.last_access_ms).max().unwrap_or(0),
            "checkouts": checkouts
        }));
    }
    projects.sort_by_key(|item| std::cmp::Reverse(item["last_access_at_ms"].as_u64().unwrap_or(0)));
    json!({
        "ok": true,
        "observed_at_ms": now,
        "active_within_minutes": active_minutes,
        "recent_within_minutes": recent_minutes,
        "active_project_count": active_project_count,
        "recent_project_count": recent_project_count,
        "active_worktree_count": active_worktree_count,
        "projects": projects,
        "activity_definition": "Successful tool use observed by this MCP runtime; recent activity is not a filesystem lock or a live-agent heartbeat."
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{create_dir_all, remove_dir_all, write};

    #[test]
    fn discovers_git_directory_and_linked_worktree_marker() {
        let root = std::env::temp_dir().join(format!("ctmcp-activity-{}", uuid::Uuid::new_v4()));
        let worktree = root.join("worktrees").join("feature");
        create_dir_all(&worktree).unwrap();
        create_dir_all(root.join(".git")).unwrap();
        write(
            worktree.join(".git"),
            "gitdir: ../../.git/worktrees/feature",
        )
        .unwrap();
        // The tool only ever reports roots within the configured folder.
        let folder = WorkspaceFolder {
            id: "workspace".to_string(),
            name: "workspace".to_string(),
            path: root.to_string_lossy().into_owned(),
            execution: crate::workspace::ExecutionTarget::Host,
        };
        assert_eq!(
            checkout_for(&folder, "git_status", &json!({"path":"."})).1,
            "repository"
        );
        assert_eq!(
            checkout_for(&folder, "read_file", &json!({"path":"worktrees/feature"})).1,
            "worktree"
        );
        let submodule = root.join("modules").join("nested");
        create_dir_all(&submodule).unwrap();
        write(submodule.join(".git"), "gitdir: ../../.git/modules/nested").unwrap();
        assert_eq!(
            checkout_for(&folder, "read_file", &json!({"path":"modules/nested"})).1,
            "repository"
        );
        remove_dir_all(root).unwrap();
    }
}
