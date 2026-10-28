use std::path::PathBuf;

use serde_json::{json, Value};
use tauri::State;

use crate::app_state::AppState;
use crate::error::{AppError, AppResult};
use crate::workspace::WorkspaceProfile;

fn history_root(profile: &WorkspaceProfile, folder_id: Option<&str>) -> AppResult<PathBuf> {
    if let Some(folder_id) = folder_id {
        let folder = profile
            .folders
            .iter()
            .find(|folder| folder.id == folder_id)
            .ok_or_else(|| AppError::Message(format!("folder not found: {folder_id}")))?;
        return Ok(PathBuf::from(&folder.path));
    }
    if let Some(folder) = profile.active_folder() {
        return Ok(PathBuf::from(&folder.path));
    }
    if !profile.path.trim().is_empty() {
        return Ok(PathBuf::from(&profile.path));
    }
    Err(AppError::Message(
        "workspace has no configured folder".into(),
    ))
}

fn workspace_profile(state: &AppState, id: &str) -> AppResult<WorkspaceProfile> {
    state.with_workspaces(|store| {
        store
            .get(id)
            .cloned()
            .ok_or_else(|| AppError::Message(format!("workspace not found: {id}")))
    })
}

#[tauri::command]
pub fn list_history_sessions(
    state: State<'_, AppState>,
    id: String,
    folder_id: Option<String>,
) -> AppResult<Value> {
    let profile = workspace_profile(&state, &id)?;
    let root = history_root(&profile, folder_id.as_deref())?;
    crate::tools::history::list_for_ui(&root, Some(&profile.id))
        .map_err(|error| AppError::Message(error.message()))
}

#[tauri::command]
pub fn read_history_session(
    state: State<'_, AppState>,
    id: String,
    number: u64,
    folder_id: Option<String>,
) -> AppResult<Value> {
    let profile = workspace_profile(&state, &id)?;
    let root = history_root(&profile, folder_id.as_deref())?;
    crate::tools::history::read_for_ui(&root, number)
        .map_err(|error| AppError::Message(error.message()))
}

fn selected_history_context(
    state: &AppState,
    id: &str,
    number: u64,
    folder_id: Option<&str>,
) -> AppResult<(String, String, Option<String>)> {
    let profile = workspace_profile(state, id)?;
    let root = history_root(&profile, folder_id)?;
    let selected_folder = folder_id
        .and_then(|selected| profile.folders.iter().find(|folder| folder.id == selected))
        .or_else(|| profile.active_folder())
        .ok_or_else(|| AppError::Message("History folder is not configured".into()))?;
    let session_key = crate::tools::history::session_key_for_ui(&root, number)
        .map_err(|error| AppError::Message(error.message()))?;
    Ok((profile.id.clone(), selected_folder.id.clone(), session_key))
}

#[tauri::command]
pub fn list_history_background_jobs(
    state: State<'_, AppState>,
    id: String,
    number: u64,
    folder_id: Option<String>,
) -> AppResult<Value> {
    let (profile_id, folder_id, session_key) =
        selected_history_context(&state, &id, number, folder_id.as_deref())?;
    let Some(session_key) = session_key else {
        return Ok(json!({
            "jobs": [],
            "recentActions": [],
            "capturedAtMs": crate::mcp::session_activity_now_ms(),
        }));
    };
    let mut jobs = crate::mcp::session_background_job_refs(&profile_id, &session_key)
        .into_iter()
        .filter_map(|job| {
            let session =
                crate::tools::hub::history_process_in_folder(&profile_id, &folder_id, &job.id)?;
            if session.is_finalized() {
                return None;
            }
            let summary = session.summary();
            let status = summary.get("status")?.as_str()?;
            if status != "running" && status != "verifying" {
                return None;
            }
            let started_at_ms = summary
                .get("started_ts_ms")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            Some(json!({
                "id": job.id,
                "tool": job.tool,
                "action": job.action,
                "status": status,
                "startedAtMs": started_at_ms,
                "elapsedMs": crate::mcp::session_activity_now_ms().saturating_sub(started_at_ms),
            }))
        })
        .collect::<Vec<_>>();
    jobs.sort_by_key(|job| std::cmp::Reverse(job["startedAtMs"].as_u64().unwrap_or(0)));
    jobs.truncate(64);
    Ok(json!({
        "jobs": jobs,
        "recentActions": crate::mcp::session_recent_actions(&profile_id, &session_key),
        "capturedAtMs": crate::mcp::session_activity_now_ms(),
    }))
}

#[tauri::command]
pub fn read_history_background_log(
    state: State<'_, AppState>,
    id: String,
    number: u64,
    folder_id: Option<String>,
    job_id: String,
) -> AppResult<Value> {
    let (profile_id, folder_id, session_key) =
        selected_history_context(&state, &id, number, folder_id.as_deref())?;
    let session_key = session_key.ok_or_else(|| {
        AppError::Message("Background job not found in this history session".into())
    })?;
    let owned = crate::mcp::session_background_job_refs(&profile_id, &session_key)
        .iter()
        .any(|job| job.id == job_id);
    let session = if owned {
        crate::tools::hub::history_process_in_folder(&profile_id, &folder_id, &job_id)
    } else {
        None
    }
    .ok_or_else(|| AppError::Message("Background job not found in this history session".into()))?;
    let result = session.snapshot(16_384);
    let (stdout, _) = crate::tools::redaction::redact_sensitive_text(
        result
            .get("stdout")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let (stderr, _) = crate::tools::redaction::redact_sensitive_text(
        result
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    Ok(json!({
        "jobId": job_id,
        "status": result.get("status").and_then(Value::as_str).unwrap_or("exited"),
        "stdout": stdout,
        "stderr": stderr,
        "stdoutTruncated": result["stdout_truncated"].as_bool().unwrap_or(false),
        "stderrTruncated": result["stderr_truncated"].as_bool().unwrap_or(false),
        "capturedAtMs": crate::mcp::session_activity_now_ms(),
    }))
}
