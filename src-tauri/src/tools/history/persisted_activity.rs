use std::collections::HashMap;

use serde::Serialize;

const ACTIVITY_HEADING: &str = "## 自動擷取的工具活動（非工作結論）";
const START: &str = "<!-- coding-tools-mcp:auto-activity:start -->";
const END: &str = "<!-- coding-tools-mcp:auto-activity:end -->";
const CHECKPOINT: &str = "## 本轮检查点";
const MAX_ROWS: usize = 48;
const MAX_ACTIONS: usize = 16;
const CLOCK_SKEW_MS: u64 = 30_000;

/// Observations are historical evidence, never proof a process is still running.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PersistedToolAction {
    pub sequence: usize,
    pub tool: String,
    pub action: String,
    pub started_at_ms: u64,
    pub updated_at_ms: u64,
    pub status: String,
}

fn from_utc_node_iso(value: &str) -> Option<u64> {
    // Node's Date#toISOString always emits this 24-byte UTC shape.
    let b = value.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
    {
        return None;
    }
    let parse = |range: std::ops::Range<usize>| value.get(range)?.parse::<i64>().ok();
    let y = parse(0..4)?;
    let month = parse(5..7)?;
    let day = parse(8..10)?;
    let hour = parse(11..13)?;
    let minute = parse(14..16)?;
    let second = parse(17..19)?;
    let millisecond = parse(20..23)?;
    if !(1970..=9999).contains(&y)
        || !(1..=12).contains(&month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
        || !(0..=999).contains(&millisecond)
    {
        return None;
    }
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day < 1 || day > month_days[(month - 1) as usize] {
        return None;
    }
    // Constant-time Gregorian civil calendar conversion, days from 1970-01-01.
    let y = y - i64::from(month <= 2);
    let era = y / 400;
    let yoe = y - era * 400;
    let mp = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    if days < 0 {
        return None;
    }
    let ms = ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000 + millisecond;
    u64::try_from(ms).ok()
}

fn parse_timestamp(value: &str) -> Option<u64> {
    if let Some(value) = value.strip_prefix("unix:") {
        let (sec, fraction) = value.split_once('.').unwrap_or((value, ""));
        if !fraction.is_empty()
            && (fraction.len() != 3 || !fraction.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        let sec = sec.parse::<u64>().ok()?;
        let millis = if fraction.is_empty() {
            0
        } else {
            fraction.parse::<u64>().ok()?
        };
        return sec.checked_mul(1_000)?.checked_add(millis);
    }
    from_utc_node_iso(value)
}

fn parse_row(
    row: &str,
    sequence: usize,
    now_ms: u64,
    window_ms: u64,
) -> Option<(String, PersistedToolAction)> {
    if row.len() > 768 {
        return None;
    }
    let line = row.strip_prefix("- ")?;
    let (entry, trailing) = line.rsplit_once(" <!-- ctmcp-activity:")?;
    let id = trailing.strip_suffix(" -->")?;
    if id.len() > 100
        || !(id.starts_with("node:") || id.starts_with("rust:"))
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b':' | b'-' | b'_' | b'.'))
    {
        return None;
    }
    let mut parts = entry.splitn(3, " · ");
    let timestamp = parse_timestamp(parts.next()?)?;
    let label = parts.next()?;
    let description = parts.next()?;
    if label.is_empty()
        || label.len() > 96
        || description.len() > 600
        || timestamp == 0
        || timestamp > now_ms.saturating_add(CLOCK_SKEW_MS)
        || now_ms.saturating_sub(timestamp) > window_ms
    {
        return None;
    }
    let tool = description.split(" · ").next()?.trim();
    if tool.is_empty()
        || tool.len() > 64
        || !tool
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return None;
    }
    // Both writers prefix the tool; de-duplicate only if repeated by summary.
    let rest = description.strip_prefix(tool)?.strip_prefix(" · ");
    let action = rest
        .filter(|tail| tail.starts_with(&format!("{tool} · ")))
        .unwrap_or(description);
    let suffix = id.rsplit(':').next().unwrap_or("");
    let status = match suffix {
        "success" => "success",
        "failed" | "rpc_error" | "tool_error" | "worker_failed" => "failed",
        "cancelled" => "cancelled",
        _ => "observed", // Includes historical running/background_running.
    };
    let id_key = match suffix {
        "running" | "success" | "failed" | "rpc_error" | "tool_error" | "worker_failed"
        | "cancelled" | "background_running" | "background_ended" => {
            id.rsplit_once(':').map(|(key, _)| key).unwrap_or(id)
        }
        _ => id,
    };
    let clamped = timestamp.min(now_ms);
    Some((
        id_key.to_string(),
        PersistedToolAction {
            sequence,
            tool: tool.to_string(),
            action: action.to_string(),
            started_at_ms: clamped,
            updated_at_ms: clamped,
            status: status.to_string(),
        },
    ))
}

/// Recover actual tool activity emitted by either Node or Rust. Never infer
/// recency from an archive's Updated field, a stale process ID, or a checkpoint.
pub(crate) fn recent_activity(
    content: &str,
    now_ms: u64,
    window_ms: u64,
) -> Vec<PersistedToolAction> {
    let Some(heading) = content.find(ACTIVITY_HEADING) else {
        return Vec::new();
    };
    let Some(start) = content[heading..]
        .find(START)
        .map(|offset| heading + offset + START.len())
    else {
        return Vec::new();
    };
    let Some(end) = content[start..].find(END).map(|offset| start + offset) else {
        return Vec::new();
    };
    if content
        .find(CHECKPOINT)
        .is_some_and(|checkpoint| end > checkpoint)
    {
        return Vec::new();
    }
    let mut by_event: HashMap<String, PersistedToolAction> = HashMap::new();
    for (index, row) in content[start..end].lines().rev().take(MAX_ROWS).enumerate() {
        if let Some((key, action)) = parse_row(row, MAX_ROWS - index, now_ms, window_ms) {
            let previous = by_event.get(&key);
            if previous.is_none_or(|old| old.updated_at_ms <= action.updated_at_ms) {
                by_event.insert(key, action);
            }
        }
    }
    let mut events = by_event.into_values().collect::<Vec<_>>();
    events.sort_by(|a, b| {
        b.updated_at_ms
            .cmp(&a.updated_at_ms)
            .then_with(|| b.sequence.cmp(&a.sequence))
    });
    events.truncate(MAX_ACTIONS);
    events
}

#[cfg(test)]
mod tests {
    use super::*;

    fn section(lines: &[String]) -> String {
        format!(
            "# Session\n\n{ACTIVITY_HEADING}\n\n{START}\n{}\n{END}\n\n{CHECKPOINT}\n",
            lines.join("\n")
        )
    }

    #[test]
    fn reads_rust_and_node_activity_with_same_timestamps_and_outcomes() {
        let node_ts = "2026-12-30T07:33:20.100Z";
        // Use the parser's own date conversion to verify exact calendar math.
        assert_eq!(parse_timestamp(node_ts), Some(1_798_616_000_100));
        let now = 1_798_616_060_100;
        let events = section(&[
            format!("- {node_ts} · 請求完成 · read_file · read_file · src/main.ts <!-- ctmcp-activity:node:5:1:2:success -->"),
            format!("- unix:{:.3} · 呼叫中 · exec_command <!-- ctmcp-activity:rust:5:2:3:running -->", now as f64 / 1000.0 - 0.5),
            format!("- unix:{:.3} · 請求完成 · exec_command <!-- ctmcp-activity:rust:5:2:3:success -->", now as f64 / 1000.0),
        ]);
        let results = recent_activity(&events, now, 5 * 60_000);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].status, "success");
        assert_eq!(results[1].tool, "read_file");
        assert_eq!(results[1].status, "success");
        assert_eq!(results[1].action, "read_file · src/main.ts");
    }

    #[test]
    fn ignores_old_future_malformed_or_incomplete_markdown() {
        let now = 1_800_000_000_000;
        let data = section(&[
            "- unix:1.000 · 請求完成 · exec_command <!-- ctmcp-activity:rust:1:1:success -->".into(),
            "- unix:1800000100.000 · 請求完成 · exec_command <!-- ctmcp-activity:rust:1:2:success -->".into(),
            "- unix:1800000000.000 · 呼叫中 · read_file <!-- ctmcp-activity:rust:1:3:running -->".into(),
            "- unix:1800000000.000 · 請求完成 · read_file <!-- ctmcp-activity:node:1:4:success -->".into(),
            "- unix:1800000000.000 · 請求完成 · secret.txt <!-- ctmcp-activity:untrusted:1:5:success -->".into(),
        ]);
        let events = recent_activity(&data, now, 300_000);
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|item| item.status == "observed"));
        assert!(events.iter().any(|item| item.status == "success"));
        assert_eq!(
            recent_activity(&data.replace(END, ""), now, 300_000).len(),
            0
        );
    }

    #[test]
    fn caps_memory_and_does_not_infer_activity_from_updated_metadata() {
        let now = 1_800_000_000_000;
        let events = (0..60).map(|i| format!(
            "- unix:1800000000.000 · 請求完成 · exec_command <!-- ctmcp-activity:node:9:{i}:1:success -->"
        )).collect::<Vec<_>>();
        let doc = section(&events);
        assert_eq!(recent_activity(&doc, now, 300_000).len(), MAX_ACTIONS);
        let doc = "# Session\n\n**Updated:** unix:1800000000\n\n## 本轮检查点";
        assert!(recent_activity(doc, now, 300_000).is_empty());
    }
}
