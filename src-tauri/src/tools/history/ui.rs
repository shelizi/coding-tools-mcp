use std::path::Path;

use serde_json::{json, Value};

use super::{markdown, storage, truncate_chars, MAX_SESSION_SUMMARY_CHARS};
use crate::tools::workspace::{Workspace, WorkspaceError, WorkspaceResult};

/// Read the history archive for the desktop history viewer without changing
/// the MCP bootstrap/checkpoint lifecycle. The viewer is intentionally
/// read-only and returns only data rooted in the supplied workspace.
pub fn list_for_ui(workspace_root: &Path, profile_id: Option<&str>) -> WorkspaceResult<Value> {
    let workspace = Workspace::new(workspace_root.to_path_buf())?;
    let history_dir = storage::resolve_history_dir(&workspace, None, None)?;
    let report = storage::scan(&workspace, &history_dir)?;
    let sessions = report
        .documents
        .iter()
        .map(|document| {
            let records = markdown::parse_checkpoint_records(&document.content);
            let archive_status = markdown::metadata(&document.content, "Status")
                .unwrap_or_else(|| "active".into());
            let activity = profile_id
                .zip(document.session_key.as_deref())
                .and_then(|(profile_id, session_key)| {
                    crate::mcp::session_activity_snapshot(
                        profile_id,
                        session_key,
                        crate::mcp::session_activity_now_ms(),
                    )
                });
            let activity_status = if archive_status.eq_ignore_ascii_case("completed") {
                "completed".to_string()
            } else {
                activity
                    .as_ref()
                    .map(|snapshot| snapshot.status.clone())
                    .unwrap_or_else(|| "inactive".to_string())
            };
            json!({
                "number": document.number,
                "path": document.path,
                "title": markdown::document_title(&document.content, document.number),
                "sessionKey": document.session_key,
                "createdAt": document.created_at,
                "updatedAt": document.updated_at,
                "status": archive_status,
                "activityStatus": activity_status,
                "activityTool": activity.as_ref().map(|snapshot| snapshot.tool.clone()),
                "activityDescription": activity.as_ref().map(|snapshot| snapshot.action.clone()),
                "lastActivityAtMs": activity.as_ref().map(|snapshot| snapshot.last_activity_at_ms),
                "activeRequestCount": activity.as_ref().map(|snapshot| snapshot.active_request_count).unwrap_or(0),
                "lastActivityOutcome": activity.as_ref().map(|snapshot| snapshot.last_outcome.clone()),
                "summary": truncate_chars(&markdown::summary(&document.content), MAX_SESSION_SUMMARY_CHARS),
                "checkpointCount": records.len(),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "historyDir": crate::tools::workspace::relative_display(workspace.root(), &history_dir),
        "sessions": sessions,
        "count": report.documents.len(),
        "missingNumbers": report.missing_numbers,
        "invalidFiles": report.invalid_files,
        "emptyFiles": report.empty_files,
    }))
}

/// Read one numbered history document and expose its structured checkpoint
/// records for the desktop viewer. `number` is parsed as a number, so callers
/// cannot use this endpoint to traverse outside the archive directory.
pub fn read_for_ui(workspace_root: &Path, number: u64) -> WorkspaceResult<Value> {
    if number == 0 {
        return Err(WorkspaceError::invalid_argument(
            "History session number must be positive",
        ));
    }
    let workspace = Workspace::new(workspace_root.to_path_buf())?;
    let history_dir = storage::resolve_history_dir(&workspace, None, None)?;
    let report = storage::scan(&workspace, &history_dir)?;
    let document = report
        .documents
        .iter()
        .find(|document| document.number == number)
        .ok_or_else(|| WorkspaceError::not_found(format!("History session not found: {number}")))?;
    let records = markdown::parse_checkpoint_records(&document.content)
        .into_iter()
        .map(|record| {
            json!({
                "turnId": record.turn_id,
                "timestamp": record.timestamp,
                "userIntent": record.user_intent,
                "findings": record.findings,
                "decisions": record.decisions,
                "filesChanged": record.files_changed,
                "tests": record.tests,
                "runtimeState": record.runtime_state,
                "remainingIssues": record.remaining_issues,
                "nextActions": record.next_actions,
                "notes": record.notes,
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "number": document.number,
        "path": document.path,
        "title": markdown::document_title(&document.content, document.number),
        "sessionKey": document.session_key,
        "createdAt": document.created_at,
        "updatedAt": document.updated_at,
        "status": markdown::metadata(&document.content, "Status").unwrap_or_else(|| "active".into()),
        "summary": truncate_chars(&markdown::summary(&document.content), MAX_SESSION_SUMMARY_CHARS),
        "records": records,
        "content": document.content,
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{list_for_ui, read_for_ui};

    #[test]
    fn viewer_lists_sessions_and_structured_checkpoints() {
        let workspace = tempfile::tempdir().expect("workspace");
        let history_dir = workspace.path().join("docs/history-session");
        fs::create_dir_all(&history_dir).expect("history directory");
        fs::write(
            history_dir.join("1.md"),
            r###"# 会话 1：UI 功能

**Session key:** chat-1
**Created:** unix:1
**Updated:** unix:2
**Status:** active

## 用户核心目标

- 查看历史

## 本轮检查点

### turn-1

```json
{
  "turn_id": "turn-1",
  "timestamp": "unix:2",
  "user_intent": "查看历史",
  "findings": ["档案已存在"],
  "decisions": [],
  "files_changed": [],
  "tests": ["cargo test"],
  "runtime_state": [],
  "remaining_issues": [],
  "next_actions": [],
  "notes": ""
}
```
"###,
        )
        .expect("history document");

        let listed = list_for_ui(workspace.path(), None).expect("list history");
        assert_eq!(listed["count"], 1);
        assert_eq!(listed["sessions"][0]["checkpointCount"], 1);
        assert_eq!(listed["sessions"][0]["title"], "UI 功能");
        assert_eq!(listed["sessions"][0]["activityStatus"], "inactive");

        let detail = read_for_ui(workspace.path(), 1).expect("read history");
        assert_eq!(detail["records"][0]["turnId"], "turn-1");
        assert_eq!(detail["records"][0]["tests"][0], "cargo test");
        assert!(read_for_ui(workspace.path(), 0).is_err());
    }

    #[test]
    fn viewer_preserves_explicit_completed_status_without_runtime_activity() {
        let workspace = tempfile::tempdir().expect("workspace");
        let history_dir = workspace.path().join("docs/history-session");
        fs::create_dir_all(&history_dir).expect("history directory");
        fs::write(
            history_dir.join("1.md"),
            "# Session 1: Done\n\n**Session key:** completed-session\n**Created:** unix:1\n**Updated:** unix:2\n**Status:** completed\n",
        )
        .expect("history document");

        let listed = list_for_ui(workspace.path(), Some("completed-profile"))
            .expect("list completed history");
        assert_eq!(listed["sessions"][0]["status"], "completed");
        assert_eq!(listed["sessions"][0]["activityStatus"], "completed");
    }
}
