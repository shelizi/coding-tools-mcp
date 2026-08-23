mod markdown;
mod model;
mod storage;
mod ui;

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, WorkspaceError, WorkspaceResult};

use self::model::{HistoryDocument, IndexEntry};

pub use ui::{list_for_ui, read_for_ui};

const HISTORY_SUMMARY_WINDOW: usize = 12;
const HISTORY_NUMBER_WINDOW: usize = 256;
const MAX_SESSION_SUMMARY_CHARS: usize = 3_000;
const MAX_ALL_HISTORY_SUMMARY_CHARS: usize = 24_000;
const MAX_LATEST_HANDOFF_CHARS: usize = 24_000;
const MAX_COMPACT_ALL_HISTORY_SUMMARY_CHARS: usize = 8_000;
const MAX_COMPACT_LATEST_HANDOFF_CHARS: usize = 8_000;

#[derive(Clone)]
struct HistorySummaryEntry {
    number: u64,
    path: String,
    summary: String,
    content_sha256: String,
    content_bytes: u64,
}

pub fn bootstrap(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let (session_key, source) = resolve_session_key(args)?;
    let host_session_key_mismatch = host_session_key(args)
        .map(|host| host != session_key.as_str())
        .unwrap_or(false);
    let history_dir = resolve_dir(ctx, args)?;
    storage::ensure_directory(&history_dir)?;
    let history_lock = storage::lock_directory(&history_dir)?;
    let history_lock_wait_ms = history_lock.wait_ms();

    let mut warnings = Vec::<String>::new();
    if host_session_key_mismatch {
        warnings.push(
            "宿主会话标识与显式 session_key 不一致，已使用显式 session_key 保持会话连续。".into(),
        );
    }
    let readme = history_dir.join("README.md");
    if readme.exists() {
        fs::read_to_string(&readme).map_err(|error| {
            history_error(
                "HISTORY_READ_FAILED",
                &error.to_string(),
                "filesystem",
                true,
                json!({"path": "docs/history-session/README.md"}),
            )
        })?;
    } else {
        warnings.push("docs/history-session/README.md 不存在。".into());
    }

    let (mut index, sequence_valid, index_rebuilt, history_read_mode) = match storage::read_index(
        &history_dir,
    ) {
        Ok(Some(index)) => (index, true, false, "indexed_summary_cache_plus_latest"),
        Ok(None) => {
            warnings.push("历史索引缺失，已根据 Markdown 重建。".into());
            let report = storage::scan(&ctx.workspace, &history_dir)?;
            reject_ambiguous_history(&report)?;
            if !report.missing_numbers.is_empty() {
                return Err(history_error(
                        "HISTORY_SEQUENCE_CONFLICT",
                        "History numbering contains gaps; run history_session_validate before creating a session.",
                        "validation",
                        true,
                        json!({"missing_numbers": report.missing_numbers}),
                    ));
            }
            let sequence_valid = report.sequence_valid();
            (
                storage::rebuild_index(&report),
                sequence_valid,
                true,
                "scan_rebuild_recent_summaries_plus_latest_bounded",
            )
        }
        Err(_) => {
            warnings.push("历史索引损坏，已根据 Markdown 重建。".into());
            let report = storage::scan(&ctx.workspace, &history_dir)?;
            reject_ambiguous_history(&report)?;
            if !report.missing_numbers.is_empty() {
                return Err(history_error(
                        "HISTORY_SEQUENCE_CONFLICT",
                        "History numbering contains gaps; run history_session_validate before creating a session.",
                        "validation",
                        true,
                        json!({"missing_numbers": report.missing_numbers}),
                    ));
            }
            let sequence_valid = report.sequence_valid();
            (
                storage::rebuild_index(&report),
                sequence_valid,
                true,
                "scan_rebuild_recent_summaries_plus_latest_bounded",
            )
        }
    };

    let response_mode = if args.get("response_mode").and_then(Value::as_str) == Some("full") {
        "full"
    } else {
        "compact"
    };

    let mut indexed_prior = index
        .sessions
        .iter()
        .filter(|(key, _)| key.as_str() != session_key)
        .map(|(key, entry)| (key.clone(), entry.clone()))
        .collect::<Vec<_>>();
    indexed_prior.sort_by_key(|(_, entry)| entry.number);
    let history_count = indexed_prior.len();
    let history_numbers_omitted_count = history_count.saturating_sub(HISTORY_NUMBER_WINDOW);
    let history_number_start = indexed_prior.len().saturating_sub(HISTORY_NUMBER_WINDOW);
    let history_numbers = indexed_prior[history_number_start..]
        .iter()
        .map(|(_, entry)| entry.number)
        .collect::<Vec<_>>();
    let history_omitted_count = history_count.saturating_sub(HISTORY_SUMMARY_WINDOW);
    let recent_start = indexed_prior.len().saturating_sub(HISTORY_SUMMARY_WINDOW);
    let recent_prior = &indexed_prior[recent_start..];
    let latest_prior_key = recent_prior.last().map(|(key, _)| key.clone());
    let mut loaded_history_bytes = 0_u64;
    let mut index_cache_updated = false;
    let latest_document = if let Some((key, entry)) = recent_prior.last() {
        let document = load_indexed_document(ctx, &history_dir, key, entry)?;
        loaded_history_bytes = loaded_history_bytes.saturating_add(document.content.len() as u64);
        if let Some(index_entry) = index.sessions.get_mut(key) {
            index_cache_updated |=
                storage::update_index_entry_cache(index_entry, &document.content);
        }
        Some(document)
    } else {
        None
    };
    let mut summary_entries = Vec::with_capacity(recent_prior.len());
    for (key, fallback_entry) in recent_prior {
        let entry = index
            .sessions
            .get(key)
            .cloned()
            .unwrap_or_else(|| fallback_entry.clone());
        if latest_prior_key.as_deref() == Some(key.as_str()) || !entry.summary.is_empty() {
            let cached = index.sessions.get(key).unwrap_or(&entry);
            summary_entries.push(HistorySummaryEntry {
                number: cached.number,
                path: cached.path.clone(),
                summary: cached.summary.clone(),
                content_sha256: cached.content_sha256.clone(),
                content_bytes: cached.content_bytes,
            });
            continue;
        }
        let summary = storage::read_summary(&ctx.workspace, &history_dir, &entry)?;
        if summary.session_key.as_deref() != Some(key.as_str()) {
            return Err(history_error(
                "HISTORY_INDEX_STALE",
                "History index session_key does not match the indexed Markdown file.",
                "validation",
                true,
                json!({
                    "session_key": key,
                    "path": entry.path,
                    "document_session_key": summary.session_key
                }),
            ));
        }
        loaded_history_bytes = loaded_history_bytes.saturating_add(summary.bytes_read);
        if let Some(index_entry) = index.sessions.get_mut(key) {
            index_entry.summary = summary.summary.clone();
            index_entry.content_bytes = summary.content_bytes;
            index_cache_updated = true;
        }
        summary_entries.push(HistorySummaryEntry {
            number: entry.number,
            path: entry.path,
            summary: summary.summary,
            content_sha256: entry.content_sha256,
            content_bytes: summary.content_bytes,
        });
    }
    let total_history_bytes = summary_entries
        .iter()
        .map(|entry| entry.content_bytes)
        .sum::<u64>();

    let existing_entry = index.sessions.get(&session_key).cloned();
    let (current_number, current_path, current_content, created, resumed) = if let Some(entry) =
        existing_entry
    {
        let current_content = if response_mode == "full" {
            let document = load_indexed_document(ctx, &history_dir, &session_key, &entry)?;
            if let Some(index_entry) = index.sessions.get_mut(&session_key) {
                index_cache_updated |=
                    storage::update_index_entry_cache(index_entry, &document.content);
            }
            Some(document.content)
        } else {
            None
        };
        (entry.number, entry.path, current_content, false, true)
    } else {
        if !args
            .get("create_if_missing")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            return Err(history_error(
                "SESSION_NOT_BOOTSTRAPPED",
                "No history mapping exists for this session_key.",
                "not_found",
                false,
                json!({"session_key_source": source}),
            ));
        }
        let number = index.latest_number.saturating_add(1);
        let relative_path = format!("{}/{number}.md", history_dir_display(ctx, &history_dir));
        let timestamp = now_timestamp();
        let title = args
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("开发会话");
        let inherited_summary = build_inherited_summary(&summary_entries, history_omitted_count);
        let content = markdown::attach_inherited_summary(
            markdown::render_document(
                number,
                title,
                &session_key,
                &timestamp,
                &timestamp,
                "active",
                &[],
            ),
            &inherited_summary,
        );
        storage::write_markdown(&history_dir.join(format!("{number}.md")), &content)?;
        index.latest_number = number;
        let mut entry = IndexEntry {
            number,
            path: relative_path.clone(),
            created_at: timestamp.clone(),
            updated_at: timestamp,
            summary: String::new(),
            content_sha256: String::new(),
            content_bytes: 0,
        };
        storage::update_index_entry_cache(&mut entry, &content);
        index.sessions.insert(session_key.clone(), entry);
        storage::write_index(&history_dir, &index)?;
        (number, relative_path, Some(content), true, false)
    };
    let mut history_read_mode = history_read_mode;
    if !index_rebuilt && index_cache_updated {
        history_read_mode = "indexed_summary_cache_backfill_plus_latest";
    }
    if (index_rebuilt || index_cache_updated) && !created {
        storage::write_index(&history_dir, &index)?;
    }
    let session_summaries = summary_entries
        .iter()
        .map(|entry| {
            json!({
                "number": entry.number,
                "path": entry.path,
                "summary": truncate_chars(&entry.summary, MAX_SESSION_SUMMARY_CHARS)
            })
        })
        .collect::<Vec<_>>();
    let all_history_summary = truncate_chars(
        &session_summaries
            .iter()
            .map(|summary| {
                format!(
                    "会话 {}（{}）：{}",
                    summary["number"].as_u64().unwrap_or_default(),
                    summary["path"].as_str().unwrap_or_default(),
                    summary["summary"].as_str().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        if response_mode == "full" {
            MAX_ALL_HISTORY_SUMMARY_CHARS
        } else {
            MAX_COMPACT_ALL_HISTORY_SUMMARY_CHARS
        },
    );
    let latest = summary_entries.last();
    let latest_handoff_limit = if response_mode == "full" {
        MAX_LATEST_HANDOFF_CHARS
    } else {
        MAX_COMPACT_LATEST_HANDOFF_CHARS
    };
    let latest_handoff_was_truncated = latest_document
        .as_ref()
        .is_some_and(|document| document.content.chars().count() > latest_handoff_limit);
    let latest_handoff = latest_document
        .as_ref()
        .map(|document| truncate_chars(&document.content, latest_handoff_limit));
    let inherited_summary = current_content
        .as_deref()
        .and_then(markdown::inherited_summary);
    let compact_sections_omitted = response_mode == "compact"
        && (!session_summaries.is_empty() || inherited_summary.is_some());
    let inherited_summary_response = if response_mode == "full" {
        inherited_summary
    } else {
        None
    };
    let session_summaries_response = if response_mode == "full" {
        session_summaries
    } else {
        Vec::new()
    };
    let lazy_sections = if response_mode == "compact" {
        vec!["inherited_summary", "session_summaries"]
    } else {
        Vec::<&str>::new()
    };
    let assistant_instructions = if response_mode == "full" {
        "Read all_history_summary, latest_handoff, and inherited_summary before continuing the project. Preserve the session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. After completing each user-requested task, call history_session_checkpoint before the final response. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path."
    } else {
        "Use the compact all_history_summary and latest_handoff to restore context. Request history_session_bootstrap again with response_mode=\"full\" only when deeper prior-session detail is materially needed. Preserve the session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. After completing each user-requested task, call history_session_checkpoint before the final response. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path."
    };
    let mut digest = Sha256::new();
    for entry in &summary_entries {
        digest.update(entry.number.to_le_bytes());
        digest.update(entry.path.as_bytes());
        digest.update(entry.summary.as_bytes());
        digest.update(entry.content_sha256.as_bytes());
        digest.update(entry.content_bytes.to_le_bytes());
    }

    let mut payload = json!({
        "is_new_session": created,
        "session_key": session_key.clone(),
        "session_key_source": source,
        "host_session_key_mismatch": host_session_key_mismatch,
        "history_numbers": history_numbers,
        "history_numbers_omitted_count": history_numbers_omitted_count,
        "history_number_window": HISTORY_NUMBER_WINDOW,
        "history_count": history_count,
        "history_loaded_count": summary_entries.len(),
        "history_omitted_count": history_omitted_count,
        "history_summary_window": HISTORY_SUMMARY_WINDOW,
        "latest_completed_number": latest.map(|document| document.number),
        "latest_completed_path": latest.map(|document| document.path.clone()),
        "current_number": current_number,
        "current_path": current_path.clone(),
        "created": created,
        "resumed": resumed,
        "sequence_valid": sequence_valid,
        "response_mode": response_mode,
        "all_history_summary": all_history_summary
    });
    let remainder = json!({
        "inherited_summary": inherited_summary_response,
        "session_summaries": session_summaries_response,
        "lazy_sections": lazy_sections,
        "full_response_available": true,
        "latest_handoff": latest_handoff,
        "latest_handoff_truncated": latest_handoff_was_truncated,
        "payload_bounded": history_omitted_count > 0 || latest_handoff_was_truncated || compact_sections_omitted,
        "history_read_mode": history_read_mode,
        "history_lock_wait_ms": history_lock_wait_ms,
        "total_history_bytes": total_history_bytes,
        "loaded_history_bytes": loaded_history_bytes,
        "full_history_included": false,
        "history_digest": format!("{:x}", digest.finalize()),
        "persistence_mode": "model_mediated_tool_calls",
        "assistant_instructions": assistant_instructions,
        "required_next_actions": [
            "read_all_history_summary",
            "read_latest_handoff",
            "verify_workspace_state",
            "execute_user_task",
            "checkpoint_after_each_completed_task"
        ],
        "checkpoint_policy": {
            "tool": "history_session_checkpoint",
            "session_key": session_key,
            "expected_path": current_path,
            "stable_target_required": true,
            "required_before_final_response": true,
            "applies_after_bootstrap": true,
            "automatic_background_persistence": false
        },
        "warnings": warnings
    });
    if let (Some(payload), Some(remainder)) = (payload.as_object_mut(), remainder.as_object()) {
        payload.extend(remainder.clone());
    }
    Ok(tool_ok(payload))
}
fn host_session_key(args: &Value) -> Option<&str> {
    args.get("_host_session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn required_checkpoint_argument(args: &Value, name: &str) -> WorkspaceResult<String> {
    args.get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            history_error(
                "CHECKPOINT_TARGET_REQUIRED",
                "Pass session_key and expected_path exactly as returned by history_session_bootstrap.",
                "validation",
                false,
                json!({"missing_argument": name}),
            )
        })
}

pub fn checkpoint(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let session_key = required_checkpoint_argument(args, "session_key")?;
    let expected_path = required_checkpoint_argument(args, "expected_path")?;
    let host_session_key_mismatch = host_session_key(args)
        .map(|host| host != session_key.as_str())
        .unwrap_or(false);
    let history_dir = resolve_dir(ctx, args)?;
    if !history_dir.exists() {
        return Err(session_not_bootstrapped());
    }

    if let Ok(Some(index)) = storage::read_index(&history_dir) {
        let entry = index
            .sessions
            .get(&session_key)
            .ok_or_else(session_not_bootstrapped)?;
        ensure_checkpoint_target(&session_key, &expected_path, &entry.path)?;
    }

    let history_lock = storage::lock_directory(&history_dir)?;
    let history_lock_wait_ms = history_lock.wait_ms();
    let (mut index, document, history_read_mode) = match storage::read_index(&history_dir) {
        Ok(Some(index)) => {
            let entry = index
                .sessions
                .get(&session_key)
                .cloned()
                .ok_or_else(session_not_bootstrapped)?;
            ensure_checkpoint_target(&session_key, &expected_path, &entry.path)?;
            let document = load_indexed_document(ctx, &history_dir, &session_key, &entry)?;
            (index, document, "index_direct")
        }
        Ok(None) | Err(_) => {
            let report = storage::scan(&ctx.workspace, &history_dir)?;
            reject_ambiguous_history(&report)?;
            let document = report
                .documents
                .iter()
                .find(|document| document.session_key.as_deref() == Some(session_key.as_str()))
                .cloned()
                .ok_or_else(session_not_bootstrapped)?;
            ensure_checkpoint_target(&session_key, &expected_path, &document.path)?;
            (storage::rebuild_index(&report), document, "scan_rebuild")
        }
    };

    let timestamp = now_timestamp();
    let timestamp_was_explicit = args
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let mut record = markdown::checkpoint_from_args(args, &timestamp)
        .map_err(WorkspaceError::invalid_argument)?;
    let redacted = if ctx.runtime_config().policy.security_policy.redact_history {
        markdown::redact_record(&mut record)
    } else {
        false
    };
    let mut records = markdown::parse_checkpoint_records(&document.content);
    let mut duplicate_ignored = false;
    let mut updated = false;
    if let Some(existing) = records
        .iter_mut()
        .find(|existing| existing.turn_id == record.turn_id)
    {
        if !timestamp_was_explicit {
            record.timestamp.clone_from(&existing.timestamp);
        }
        if existing == &record {
            duplicate_ignored = true;
        } else {
            *existing = record.clone();
            updated = true;
        }
    } else {
        records.push(record.clone());
        updated = true;
    }

    let final_content = if duplicate_ignored {
        document.content.clone()
    } else {
        let created_at = document
            .created_at
            .clone()
            .unwrap_or_else(|| timestamp.clone());
        let inherited_summary = markdown::inherited_summary(&document.content);
        markdown::attach_inherited_summary(
            markdown::render_document(
                document.number,
                &markdown::document_title(&document.content, document.number),
                &session_key,
                &created_at,
                &record.timestamp,
                "active",
                &records,
            ),
            inherited_summary.as_deref().unwrap_or_default(),
        )
    };
    if !duplicate_ignored {
        storage::write_markdown(
            &history_dir.join(format!("{}.md", document.number)),
            &final_content,
        )?;
    }
    index.latest_number = index.latest_number.max(document.number);
    let entry = index
        .sessions
        .entry(session_key.clone())
        .or_insert_with(|| IndexEntry {
            number: document.number,
            path: document.path.clone(),
            created_at: document
                .created_at
                .clone()
                .unwrap_or_else(|| timestamp.clone()),
            updated_at: record.timestamp.clone(),
            summary: String::new(),
            content_sha256: String::new(),
            content_bytes: 0,
        });
    entry.number = document.number;
    entry.path = document.path.clone();
    if entry.created_at.is_empty() {
        entry.created_at = document
            .created_at
            .clone()
            .unwrap_or_else(|| timestamp.clone());
    }
    entry.updated_at = record.timestamp.clone();
    storage::update_index_entry_cache(entry, &final_content);
    let content_hash = entry.content_sha256.clone();
    storage::write_index(&history_dir, &index)?;

    let mut warnings = Vec::new();
    if redacted {
        warnings.push("检测到疑似敏感信息，归档内容已脱敏。");
    }
    if host_session_key_mismatch {
        warnings.push("宿主会话标识已变化；本次仍使用 bootstrap 返回的稳定目标，未切换历史文件。");
    }
    Ok(tool_ok(json!({
        "session_number": document.number,
        "path": document.path,
        "session_key": session_key,
        "expected_path": expected_path,
        "host_session_key_mismatch": host_session_key_mismatch,
        "turn_id": record.turn_id,
        "created": false,
        "updated": updated,
        "duplicate_ignored": duplicate_ignored,
        "content_hash": content_hash,
        "history_read_mode": history_read_mode,
        "history_lock_wait_ms": history_lock_wait_ms,
        "warnings": warnings
    })))
}
pub fn validate(ctx: &ToolContext, args: &Value) -> WorkspaceResult<Value> {
    let history_dir = resolve_dir(ctx, args)?;
    let repair = args.get("repair").and_then(Value::as_bool).unwrap_or(false);
    if repair {
        storage::ensure_directory(&history_dir)?;
    }
    let mut index_status = "missing";
    if history_dir.exists() {
        index_status = match storage::read_index(&history_dir) {
            Ok(Some(_)) => "valid",
            Ok(None) => "missing",
            Err(_) => "invalid",
        };
    }
    let report = storage::scan(&ctx.workspace, &history_dir)?;
    let mut warnings = Vec::<String>::new();
    if !report.duplicate_session_keys.is_empty() {
        warnings.push("存在重复 session_key，相关映射未写入索引。".into());
    }
    let mut history_lock_wait_ms = 0_u128;
    let repaired = if repair {
        let history_lock = storage::lock_directory(&history_dir)?;
        history_lock_wait_ms = history_lock.wait_ms();
        let locked_report = storage::scan(&ctx.workspace, &history_dir)?;
        storage::write_index(&history_dir, &storage::rebuild_index(&locked_report))?;
        true
    } else {
        false
    };
    let latest_number = report.latest_number();
    let latest_path = latest_number.and_then(|number| {
        report
            .documents
            .iter()
            .find(|document| document.number == number)
            .map(|document| document.path.clone())
    });
    Ok(tool_ok(json!({
        "sequence_valid": report.sequence_valid(),
        "numbers": report.numbers,
        "missing_numbers": report.missing_numbers,
        "duplicate_session_keys": report.duplicate_session_keys,
        "invalid_files": report.invalid_files,
        "empty_files": report.empty_files,
        "latest_number": latest_number,
        "latest_path": latest_path,
        "index_status": index_status,
        "repaired": repaired,
        "history_lock_wait_ms": history_lock_wait_ms,
        "warnings": warnings
    })))
}

fn load_indexed_document(
    ctx: &ToolContext,
    history_dir: &std::path::Path,
    session_key: &str,
    entry: &IndexEntry,
) -> WorkspaceResult<HistoryDocument> {
    let document = storage::read_document(&ctx.workspace, history_dir, entry)?;
    if document.session_key.as_deref() != Some(session_key) {
        return Err(history_error(
            "HISTORY_INDEX_STALE",
            "History index session_key does not match the indexed Markdown file.",
            "validation",
            true,
            json!({
                "session_key": session_key,
                "path": entry.path,
                "document_session_key": document.session_key
            }),
        ));
    }
    Ok(document)
}

fn ensure_checkpoint_target(
    session_key: &str,
    expected_path: &str,
    resolved_path: &str,
) -> WorkspaceResult<()> {
    if expected_path == resolved_path {
        return Ok(());
    }
    Err(history_error(
        "SESSION_TARGET_MISMATCH",
        "The checkpoint target does not match the session initialized by bootstrap.",
        "validation",
        false,
        json!({
            "expected_path": expected_path,
            "resolved_path": resolved_path,
            "session_key": session_key
        }),
    ))
}

fn resolve_dir(ctx: &ToolContext, args: &Value) -> WorkspaceResult<std::path::PathBuf> {
    storage::resolve_history_dir(
        &ctx.workspace,
        args.get("workspace_root").and_then(Value::as_str),
        args.get("history_dir").and_then(Value::as_str),
    )
}

fn resolve_session_key(args: &Value) -> WorkspaceResult<(String, &'static str)> {
    if let Some(value) = args
        .get("session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok((value.to_string(), "explicit_session_key"));
    }
    if let Some(value) = args
        .get("_host_session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok((value.to_string(), "platform_conversation_id"));
    }
    if let Some(value) = args
        .get("_fallback_session_key")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok((value.to_string(), "stable_runtime_fallback"));
    }
    Err(history_error(
        "SESSION_ID_UNAVAILABLE",
        "A stable ChatGPT session identifier is required.",
        "validation",
        false,
        json!({}),
    ))
}

fn reject_ambiguous_history(report: &model::ScanReport) -> WorkspaceResult<()> {
    if report.duplicate_session_keys.is_empty() {
        return Ok(());
    }
    Err(history_error(
        "HISTORY_INDEX_CONFLICT",
        "Multiple history files declare the same session_key.",
        "validation",
        false,
        json!({"duplicate_session_keys": report.duplicate_session_keys}),
    ))
}

fn session_not_bootstrapped() -> WorkspaceError {
    history_error(
        "SESSION_NOT_BOOTSTRAPPED",
        "The session_key has not been bootstrapped.",
        "not_found",
        false,
        json!({}),
    )
}

fn history_error(
    code: &'static str,
    message: &str,
    category: &'static str,
    retryable: bool,
    details: Value,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category,
        retryable,
        details,
    }
}

fn history_dir_display(ctx: &ToolContext, path: &std::path::Path) -> String {
    crate::tools::workspace::relative_display(ctx.workspace.root(), path)
}

fn now_timestamp() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("unix:{seconds}")
}

fn build_inherited_summary(documents: &[HistorySummaryEntry], externally_omitted: usize) -> String {
    const MAX_TOTAL_CHARS: usize = 16_000;
    const MAX_SESSION_CHARS: usize = 3_000;

    let mut entries = Vec::new();
    let mut used = 0_usize;
    let mut omitted = externally_omitted;
    for document in documents.iter().rev() {
        let compact = truncate_chars(&document.summary, MAX_SESSION_CHARS);
        let entry = format!(
            "### 会话 {}（{}）\n\n{}",
            document.number, document.path, compact
        );
        let entry_len = entry.chars().count();
        if used + entry_len > MAX_TOTAL_CHARS {
            omitted += 1;
            continue;
        }
        used += entry_len;
        entries.push(entry);
    }
    entries.reverse();
    if omitted > 0 {
        entries.insert(
            0,
            format!("> 另有 {omitted} 个较早会话未展开，可通过 all_history_summary 读取。"),
        );
    }
    entries.join("\n\n")
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push_str("…（摘要已截断）");
    truncated
}
