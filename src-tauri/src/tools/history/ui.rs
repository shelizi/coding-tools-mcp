use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{markdown, storage, truncate_chars, MAX_SESSION_SUMMARY_CHARS};
use crate::tools::workspace::{Workspace, WorkspaceError, WorkspaceResult};

/// Validate the numbered history document and read only its header for the
/// session-to-background-process association. Do not scan the entire archive
/// or trust a session key supplied by the UI.
pub fn session_key_for_ui(workspace_root: &Path, number: u64) -> WorkspaceResult<Option<String>> {
    if number == 0 {
        return Err(WorkspaceError::invalid_argument(
            "History session number must be positive",
        ));
    }
    let workspace = Workspace::new(workspace_root.to_path_buf())?;
    let history_dir = storage::resolve_history_dir(&workspace, None, None)?;
    let file = workspace.resolve_existing_at(&history_dir, &format!("{number}.md"))?;
    let mut prefix = Vec::new();
    std::fs::File::open(file.path)
        .map_err(|_| WorkspaceError::not_found("History session not found"))?
        .take(16_384)
        .read_to_end(&mut prefix)
        .map_err(|_| WorkspaceError::not_found("History session could not be read"))?;
    Ok(markdown::metadata(
        &String::from_utf8_lossy(&prefix),
        "Session key",
    ))
}

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
            let now_ms = crate::mcp::session_activity_now_ms();
            let activity = profile_id
                .zip(document.session_key.as_deref())
                .and_then(|(profile_id, session_key)| {
                    crate::mcp::session_activity_snapshot(
                        profile_id,
                        session_key,
                        now_ms,
                    )
                });
            // Rust and Node own separate live MCP trackers, but both persist
            // bounded observations into the same numbered History Markdown.
            // Use either runtime's recent events for 'active', NEVER for
            // 'running' (only this process knows which commands are alive).
            let persisted_actions = super::persisted_activity::recent_activity(
                &document.content,
                now_ms,
                crate::mcp::SESSION_ACTIVE_WINDOW_MS,
            );
            let persisted_latest = persisted_actions.first();
            let runtime_at = activity.as_ref().map(|a| a.last_activity_at_ms).unwrap_or(0);
            let persisted_at = persisted_latest.map(|a| a.updated_at_ms).unwrap_or(0);
            let last_at = runtime_at.max(persisted_at);
            let running = activity.as_ref().is_some_and(|item| item.status == "running");
            let recent = last_at > 0 &&
                now_ms.saturating_sub(last_at) <= crate::mcp::SESSION_ACTIVE_WINDOW_MS;
            let activity_status = if archive_status.eq_ignore_ascii_case("completed") {
                "completed"
            } else if running {
                "running"
            } else if recent {
                "active"
            } else {
                "inactive"
            };
            let live_actions = profile_id
                .zip(document.session_key.as_deref())
                .map(|(id, key)| crate::mcp::session_recent_actions(id, key))
                .unwrap_or_default();
            let use_live = runtime_at >= persisted_at;
            let recent_actions = if use_live && !live_actions.is_empty() {
                json!(live_actions)
            } else {
                json!(persisted_actions)
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
                "activityTool": if use_live { activity.as_ref().map(|a| a.tool.as_str()) } else { persisted_latest.map(|a| a.tool.as_str()) },
                "activityDescription": if use_live { activity.as_ref().map(|a| a.action.as_str()) } else { persisted_latest.map(|a| a.action.as_str()) },
                "lastActivityAtMs": if last_at > 0 { Some(last_at) } else { None },
                "activeRequestCount": activity.as_ref().map(|snapshot| snapshot.active_request_count).unwrap_or(0),
                "lastActivityOutcome": if use_live { activity.as_ref().map(|a| a.last_outcome.as_str()) } else { persisted_latest.map(|a| a.status.as_str()) },
                "summary": truncate_chars(&markdown::summary(&document.content), MAX_SESSION_SUMMARY_CHARS),
                "checkpointCount": records.len(),
                "revision": format!("{:x}", Sha256::digest(document.content.as_bytes())),
                "recentActions": recent_actions,
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
        "revision": format!("{:x}", Sha256::digest(document.content.as_bytes())),
    }))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{list_for_ui, read_for_ui, session_key_for_ui};

    #[test]
    fn rust_history_marks_other_runtime_persisted_activity_recent_but_not_running() {
        let workspace = tempfile::tempdir().expect("workspace");
        let history_dir = workspace.path().join("docs/history-session");
        fs::create_dir_all(&history_dir).expect("history dir");
        let now_ms = crate::mcp::session_activity_now_ms();
        let recent = now_ms.saturating_sub(60_000);
        let expired = now_ms.saturating_sub(crate::mcp::SESSION_ACTIVE_WINDOW_MS + 60_000);
        for (number, stamp, archive_status, event_state) in [
            (1, recent, "active", "success"),
            (2, expired, "active", "success"),
            (3, recent, "completed", "success"),
            (4, recent, "active", "background_running"),
        ] {
            let document = format!(
                "# Session {number}\n\n**Session key:** other-runtime-{number}\n**Created:** unix:1\n**Updated:** unix:2\n**Status:** {archive_status}\n\n## 自動擷取的工具活動（非工作結論）\n\n<!-- coding-tools-mcp:auto-activity:start -->\n- unix:{:.3} · 請求完成 · read_file · read_file · src/main.ts <!-- ctmcp-activity:node:9:{number}:1:{event_state} -->\n<!-- coding-tools-mcp:auto-activity:end -->\n\n## 本轮检查点\n",
                stamp as f64 / 1000.0,
            );
            fs::write(history_dir.join(format!("{number}.md")), document).expect("write");
        }
        let list = list_for_ui(workspace.path(), Some("other-runtime-test-profile"))
            .expect("list cross runtime");
        let by_number = |number| {
            list["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .find(|session| session["number"] == number)
                .unwrap()
                .clone()
        };
        let recent_session = by_number(1);
        assert_eq!(recent_session["activityStatus"], "active");
        assert_eq!(recent_session["activeRequestCount"], 0);
        assert_eq!(recent_session["activityTool"], "read_file");
        assert_eq!(
            recent_session["activityDescription"],
            "read_file · src/main.ts"
        );
        assert_eq!(recent_session["recentActions"][0]["status"], "success");
        assert_eq!(by_number(2)["activityStatus"], "inactive");
        assert_eq!(by_number(3)["activityStatus"], "completed");
        let saved_inflight = by_number(4);
        assert_eq!(saved_inflight["activityStatus"], "active");
        assert_eq!(saved_inflight["activeRequestCount"], 0);
        assert_eq!(saved_inflight["recentActions"][0]["status"], "observed");
        assert!(!recent_session["activityDescription"]
            .as_str()
            .unwrap_or_default()
            .contains("other-runtime-1"));
    }

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
        assert_eq!(detail["revision"], listed["sessions"][0]["revision"]);
        let original = fs::read_to_string(history_dir.join("1.md")).expect("read initial");
        fs::write(
            history_dir.join("1.md"),
            original.replace("档案已存在", "档案已验证"),
        )
        .expect("update checkpoint without updating timestamps");
        let new_list = list_for_ui(workspace.path(), None).expect("latest history");
        assert_eq!(
            new_list["sessions"][0]["updatedAt"],
            listed["sessions"][0]["updatedAt"]
        );
        assert_eq!(new_list["sessions"][0]["checkpointCount"], 1);
        assert_ne!(
            new_list["sessions"][0]["revision"],
            listed["sessions"][0]["revision"]
        );
        let new_detail = read_for_ui(workspace.path(), 1).expect("read updated detail");
        assert_eq!(new_detail["revision"], new_list["sessions"][0]["revision"]);
        assert!(read_for_ui(workspace.path(), 0).is_err());
        assert_eq!(
            session_key_for_ui(workspace.path(), 1)
                .expect("safe header")
                .as_deref(),
            Some("chat-1")
        );
        assert!(session_key_for_ui(workspace.path(), 0).is_err());
        assert!(session_key_for_ui(workspace.path(), 999).is_err());
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
