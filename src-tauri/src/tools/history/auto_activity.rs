use std::path::Path;

use crate::tools::redaction::{contains_sensitive_path, redact_sensitive_text};
use crate::tools::workspace::{Workspace, WorkspaceResult};

use super::{markdown, storage};

pub const AUTO_HEADING: &str = "## 自動擷取的工具活動（非工作結論）";
const AUTO_START: &str = "<!-- coding-tools-mcp:auto-activity:start -->";
const AUTO_END: &str = "<!-- coding-tools-mcp:auto-activity:end -->";
const CHECKPOINT: &str = "## 本轮检查点";
const MAX_LINES: usize = 48;
const MAX_BYTES: usize = 14_000;

#[derive(Clone, Debug)]
pub struct AutoActivityEvent {
    pub id: String,
    pub at_ms: u64,
    pub tool: String,
    pub action: String,
    pub status: String,
}

fn existing_section(content: &str) -> Result<Option<(usize, usize, Vec<String>)>, String> {
    let Some(start) = content.find(AUTO_HEADING) else {
        return Ok(None);
    };
    let begin = content[start..]
        .find(AUTO_START)
        .map(|offset| start + offset)
        .ok_or_else(|| "Invalid automatic activity block".to_string())?;
    let end_marker = content[begin + AUTO_START.len()..]
        .find(AUTO_END)
        .map(|offset| begin + AUTO_START.len() + offset)
        .ok_or_else(|| "Unterminated automatic activity block".to_string())?;
    if content
        .find(CHECKPOINT)
        .is_some_and(|checkpoint| end_marker >= checkpoint)
    {
        return Err("Automatic activity block crossed checkpoint section".into());
    }
    let mut end = end_marker + AUTO_END.len();
    while content
        .as_bytes()
        .get(end)
        .is_some_and(|ch| *ch == b'\n' || *ch == b'\r')
    {
        end += 1;
    }
    let rows = content[begin + AUTO_START.len()..end_marker]
        .lines()
        .filter(|line| {
            line.starts_with("- ")
                && line.len() <= 768
                && line.contains("<!-- ctmcp-activity:")
                && line.ends_with(" -->")
        })
        .map(ToOwned::to_owned)
        .collect();
    Ok(Some((start, end, rows)))
}

fn safe_text(text: &str, max_chars: usize) -> String {
    if contains_sensitive_path(text) {
        return "[sensitive path]".into();
    }
    let (redacted, _) = redact_sensitive_text(text);
    redacted
        .chars()
        .map(|ch| {
            if ch.is_control()
                || matches!(
                    ch,
                    '<' | '>' | '`' | '*' | '|' | '\\' | '#' | '[' | ']' | '{' | '}'
                )
            {
                ' '
            } else {
                ch
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn event_line(event: &AutoActivityEvent) -> Option<String> {
    if event.id.is_empty()
        || event.id.len() > 100
        || !event
            .id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | ':' | '.'))
    {
        return None;
    }
    let tool = safe_text(&event.tool, 64);
    if tool.is_empty() {
        return None;
    }
    let action = safe_text(&event.action, 120);
    let label = match event.status.as_str() {
        "running" => "呼叫中",
        "success" => "請求完成",
        "failed" => "請求失敗",
        "rpc_error" => "通訊失敗",
        "tool_error" => "工具回應錯誤",
        "cancelled" => "已取消",
        "worker_failed" => "工作程序失敗",
        "background_running" => "背景程序執行中",
        "background_ended" => "背景程序已結束",
        _ => "狀態已更新",
    };
    let detail = if action.is_empty() || action == tool {
        String::new()
    } else {
        format!(" · {action}")
    };
    Some(format!(
        "- unix:{:.3} · {label} · {tool}{detail} <!-- ctmcp-activity:{} -->",
        event.at_ms as f64 / 1000.0,
        event.id,
    ))
}

fn render_block(rows: &[String]) -> String {
    format!(
        "{AUTO_HEADING}\n\n> Coding MCP 自動觀測的工具呼叫紀錄，並非 Agent 確認的結論。只保留最近紀錄，不包含命令輸出或完整參數。\n\n{AUTO_START}\n{}{AUTO_END}\n\n",
        if rows.is_empty() { String::new() } else { format!("{}\n", rows.join("\n")) },
    )
}

pub fn merge(content: &str, events: &[AutoActivityEvent]) -> Result<String, String> {
    if events.is_empty() {
        return Ok(content.into());
    }
    let block = existing_section(content)?;
    let mut rows = block
        .as_ref()
        .map(|(_, _, rows)| rows.clone())
        .unwrap_or_default();
    let mut known = rows
        .iter()
        .filter_map(|row| {
            row.split("<!-- ctmcp-activity:")
                .nth(1)
                .and_then(|end| end.strip_suffix(" -->"))
        })
        .map(str::to_owned)
        .collect::<std::collections::HashSet<_>>();
    let mut chronological = events.to_vec();
    chronological.sort_by(|a, b| a.at_ms.cmp(&b.at_ms).then_with(|| a.id.cmp(&b.id)));
    for event in &chronological {
        if known.contains(&event.id) {
            continue;
        }
        if let Some(row) = event_line(event) {
            rows.push(row);
            known.insert(event.id.clone());
        }
    }
    let mut bounded = Vec::new();
    let mut bytes = 0;
    for row in rows.into_iter().rev() {
        let row_bytes = row.len() + 1;
        if bounded.len() == MAX_LINES || bytes + row_bytes > MAX_BYTES {
            break;
        }
        bytes += row_bytes;
        bounded.push(row);
    }
    bounded.reverse();
    let section = render_block(&bounded);
    if let Some((start, end, _)) = block {
        return Ok(format!(
            "{}{}{}",
            &content[..start],
            section,
            &content[end..]
        ));
    }
    let checkpoint = content
        .find(CHECKPOINT)
        .ok_or_else(|| "No checkpoint section".to_string())?;
    Ok(format!(
        "{}{}{}",
        &content[..checkpoint],
        section,
        &content[checkpoint..]
    ))
}

/// A semantic checkpoint re-renders the whole archive; preserve auto events.
pub fn preserve(rendered: String, old: &str) -> Result<String, String> {
    let Some((start, end, _)) = existing_section(old)? else {
        return Ok(rendered);
    };
    let checkpoint = rendered
        .find(CHECKPOINT)
        .ok_or_else(|| "No checkpoint heading".to_string())?;
    Ok(format!(
        "{}{}{}",
        &rendered[..checkpoint],
        &old[start..end],
        &rendered[checkpoint..]
    ))
}

/// Writes only a verified, bootstrapped numbered Markdown entry under the shared
/// Node/Rust .history.lock.d lock; never creates a new history session.
pub fn persist(
    root: &Path,
    session_key: &str,
    events: &[AutoActivityEvent],
) -> WorkspaceResult<bool> {
    if events.is_empty() {
        return Ok(false);
    }
    let workspace = Workspace::new(root.to_path_buf())?;
    let history_dir = storage::resolve_history_dir(&workspace, None, None)?;
    if !history_dir.is_dir() {
        return Ok(false);
    }
    let _lock = storage::lock_directory(&history_dir)?;
    let Some(mut index) = storage::read_index(&history_dir)? else {
        return Ok(false);
    };
    let Some(entry) = index.sessions.get(session_key).cloned() else {
        return Ok(false);
    };
    let document = storage::read_document(&workspace, &history_dir, &entry)?;
    if document.session_key.as_deref() != Some(session_key)
        || markdown::metadata(&document.content, "Status")
            .is_some_and(|status| status.eq_ignore_ascii_case("completed"))
    {
        return Ok(false);
    }
    let next = merge(&document.content, events)
        .map_err(crate::tools::workspace::WorkspaceError::invalid_argument)?;
    if next == document.content {
        return Ok(false);
    }
    storage::write_markdown(&history_dir.join(format!("{}.md", entry.number)), &next)?;
    if let Some(entry) = index.sessions.get_mut(session_key) {
        storage::update_index_entry_cache(entry, &next);
    }
    storage::write_index(&history_dir, &index)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::super::model::{HistoryIndex, IndexEntry};
    use super::*;

    fn event(id: &str, at_ms: u64) -> AutoActivityEvent {
        AutoActivityEvent {
            id: id.to_string(),
            at_ms,
            tool: "exec_command".into(),
            action: "exec_command · cargo".into(),
            status: "success".into(),
        }
    }

    #[test]
    fn activity_is_bounded_deduplicated_and_preserved_across_checkpoint_regeneration() {
        let initial =
            markdown::render_document(1, "Work", "owner", "unix:1", "unix:1", "active", &[]);
        let events = (0..160)
            .map(|i| event(&format!("rust:10:{i}:success"), 1000 + i))
            .collect::<Vec<_>>();
        let updated = merge(&initial, &events).expect("merge");
        assert_eq!(
            updated.matches("<!-- ctmcp-activity:rust:").count(),
            MAX_LINES
        );
        assert!(!updated.contains("rust:10:0:success"));
        assert!(updated.contains("rust:10:159:success"));
        assert_eq!(
            merge(&updated, &events[112..]).expect("idempotent"),
            updated
        );
        let rendered =
            markdown::render_document(1, "Work", "owner", "unix:1", "unix:2", "active", &[]);
        let preserved = preserve(rendered, &updated).expect("keep activity section");
        assert_eq!(preserved.matches(AUTO_HEADING).count(), 1);
        assert_eq!(
            preserved.matches("<!-- ctmcp-activity:rust:").count(),
            MAX_LINES
        );
        assert!(markdown::parse_checkpoint_records(&preserved).is_empty());
    }

    #[test]
    fn auto_activity_writer_requires_real_bootstrap_mapping_and_safe_history_lock() {
        let workspace = tempfile::tempdir().expect("temporary project");
        let dir = workspace.path().join("docs/history-session");
        std::fs::create_dir_all(&dir).expect("history dir");
        let base =
            markdown::render_document(1, "Work", "session-a", "unix:1", "unix:1", "active", &[]);
        storage::write_markdown(&dir.join("1.md"), &base).expect("initial");
        let mut index = HistoryIndex {
            version: 1,
            latest_number: 1,
            ..HistoryIndex::default()
        };
        let mut entry = IndexEntry {
            number: 1,
            path: "docs/history-session/1.md".into(),
            created_at: "unix:1".into(),
            updated_at: "unix:1".into(),
            summary: String::new(),
            content_sha256: String::new(),
            content_bytes: 0,
        };
        storage::update_index_entry_cache(&mut entry, &base);
        index.sessions.insert("session-a".into(), entry);
        storage::write_index(&dir, &index).expect("index");
        assert!(
            !persist(workspace.path(), "wrong-key", &[event("rust:wrong", 1)])
                .expect("unknown key ignored")
        );
        assert!(
            persist(workspace.path(), "session-a", &[event("rust:test:1", 1000)]).expect("persist")
        );
        let updated = std::fs::read_to_string(dir.join("1.md")).expect("read");
        assert!(updated.contains("rust:test:1"));
        let indexed = storage::read_index(&dir)
            .expect("read index")
            .expect("exists");
        assert_eq!(
            indexed.sessions["session-a"].content_sha256,
            storage::sha256(updated.as_bytes())
        );
        assert!(
            !persist(workspace.path(), "session-a", &[event("rust:test:1", 1000)])
                .expect("same event")
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("1.md")).expect("again"),
            updated
        );
        // Agent checkpoint render must keep this activity as an independent block.
        let checkpoint = preserve(
            markdown::render_document(1, "Work", "session-a", "unix:1", "unix:2", "active", &[]),
            &updated,
        )
        .expect("preserve");
        assert!(checkpoint.contains("rust:test:1"));
    }

    #[test]
    fn invalid_sections_do_not_silently_erase_user_history() {
        let original =
            markdown::render_document(1, "Work", "owner", "unix:1", "unix:1", "active", &[]);
        let corrupt = original.replace(
            CHECKPOINT,
            &format!("{AUTO_HEADING}\n{AUTO_START}\ninvalid\n{CHECKPOINT}"),
        );
        assert!(merge(&corrupt, &[event("rust:1", 1000)]).is_err());
        assert!(preserve(original, &corrupt).is_err());
    }

    #[test]
    fn node_generated_lines_are_retained_by_rust_writer() {
        let initial = markdown::render_document(
            1,
            "Cross-runtime",
            "owner",
            "unix:1",
            "unix:1",
            "active",
            &[],
        );
        let node_row = "- 2026-10-23T08:00:00.000Z · 請求完成 · read_file <!-- ctmcp-activity:node:9:123:1:success -->";
        let node_section = format!(
            "{AUTO_HEADING}\n\n> Coding MCP 自動觀測的工具呼叫紀錄，並非 Agent 確認的結論。只保留最近紀錄，不包含命令輸出或完整參數。\n\n{AUTO_START}\n{node_row}\n{AUTO_END}\n\n"
        );
        let document = initial.replace(CHECKPOINT, &format!("{node_section}{CHECKPOINT}"));
        let merged =
            merge(&document, &[event("rust:9:456:success", 2000)]).expect("merge Node/Rust");
        assert!(merged.contains(node_row));
        assert!(merged.contains("rust:9:456:success"));
        assert_eq!(merged.matches(AUTO_HEADING).count(), 1);
    }
}
