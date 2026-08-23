use regex::Regex;
use serde_json::{json, Value};

use crate::tools::workspace::WorkspaceError;

#[derive(Debug, Clone)]
struct ResolvedEdit {
    input_index: usize,
    start_byte: usize,
    end_byte: usize,
    replacement: String,
}

pub(super) fn validate_precise_edit_contract(edits: &[Value]) -> Result<(), WorkspaceError> {
    let mut issues = Vec::new();
    for (edit_index, edit) in edits.iter().enumerate() {
        let Some(object) = edit.as_object() else {
            issues.push(json!({
                "edit_index": edit_index,
                "field": Value::Null,
                "reason": "edit_must_be_object"
            }));
            continue;
        };
        let Some(edit_type) = object.get("type").and_then(Value::as_str) else {
            issues.push(json!({
                "edit_index": edit_index,
                "field": "type",
                "reason": "type_required"
            }));
            continue;
        };

        let (allowed, required, non_empty_strings): (&[&str], &[&str], &[&str]) = match edit_type {
            "replace" => (
                &[
                    "type",
                    "old_text",
                    "new_text",
                    "match_mode",
                    "before_context",
                    "after_context",
                    "expected_occurrences",
                    "start_line",
                    "end_line",
                ],
                &["type", "old_text", "new_text"],
                &["old_text"],
            ),
            "insert_before" | "insert_after" => (
                &[
                    "type",
                    "anchor",
                    "text",
                    "match_mode",
                    "before_context",
                    "after_context",
                    "expected_occurrences",
                    "start_line",
                    "end_line",
                ],
                &["type", "anchor", "text"],
                &["anchor", "text"],
            ),
            "replace_lines" => (
                &[
                    "type",
                    "start_line",
                    "end_line",
                    "new_text",
                    "expected_text",
                ],
                &["type", "start_line", "end_line", "new_text"],
                &[],
            ),
            "delete_lines" => (
                &["type", "start_line", "end_line", "expected_text"],
                &["type", "start_line", "end_line"],
                &[],
            ),
            other => {
                issues.push(json!({
                    "edit_index": edit_index,
                    "field": "type",
                    "edit_type": other,
                    "reason": "unsupported_type",
                    "allowed_values": ["replace", "insert_before", "insert_after", "replace_lines", "delete_lines"]
                }));
                continue;
            }
        };

        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                issues.push(json!({
                    "edit_index": edit_index,
                    "edit_type": edit_type,
                    "field": key,
                    "reason": "unexpected_field",
                    "allowed_fields": allowed
                }));
            }
        }
        for field in required {
            if !object.contains_key(*field) {
                issues.push(json!({
                    "edit_index": edit_index,
                    "edit_type": edit_type,
                    "field": field,
                    "reason": "missing_required_field"
                }));
            }
        }

        for field in [
            "old_text",
            "new_text",
            "anchor",
            "text",
            "expected_text",
            "before_context",
            "after_context",
        ] {
            if let Some(value) = object.get(field) {
                match value.as_str() {
                    Some(text) if non_empty_strings.contains(&field) && text.is_empty() => {
                        issues.push(json!({
                            "edit_index": edit_index,
                            "edit_type": edit_type,
                            "field": field,
                            "reason": "field_must_be_non_empty"
                        }));
                    }
                    Some(_) => {}
                    None => issues.push(json!({
                        "edit_index": edit_index,
                        "edit_type": edit_type,
                        "field": field,
                        "reason": "field_must_be_string"
                    })),
                }
            }
        }

        if let Some(value) = object.get("match_mode") {
            if !matches!(value.as_str(), Some("exact" | "whitespace")) {
                issues.push(json!({
                    "edit_index": edit_index,
                    "edit_type": edit_type,
                    "field": "match_mode",
                    "reason": "invalid_enum_value",
                    "allowed_values": ["exact", "whitespace"]
                }));
            }
        }
        if let Some(value) = object.get("expected_occurrences") {
            if value.as_u64().is_none_or(|count| count == 0) {
                issues.push(json!({
                    "edit_index": edit_index,
                    "edit_type": edit_type,
                    "field": "expected_occurrences",
                    "reason": "field_must_be_positive_integer"
                }));
            }
        }

        let start_line = object.get("start_line");
        let end_line = object.get("end_line");
        for (field, value) in [("start_line", start_line), ("end_line", end_line)] {
            if let Some(value) = value {
                if value.as_u64().is_none_or(|line| line == 0) {
                    issues.push(json!({
                        "edit_index": edit_index,
                        "edit_type": edit_type,
                        "field": field,
                        "reason": "field_must_be_positive_integer"
                    }));
                }
            }
        }
        if matches!(edit_type, "replace" | "insert_before" | "insert_after")
            && start_line.is_some() != end_line.is_some()
        {
            issues.push(json!({
                "edit_index": edit_index,
                "edit_type": edit_type,
                "field": "start_line,end_line",
                "reason": "line_range_pair_required"
            }));
        }
        if let (Some(start), Some(end)) = (
            start_line.and_then(Value::as_u64),
            end_line.and_then(Value::as_u64),
        ) {
            if end < start {
                issues.push(json!({
                    "edit_index": edit_index,
                    "edit_type": edit_type,
                    "field": "end_line",
                    "reason": "line_range_order_invalid",
                    "start_line": start,
                    "end_line": end
                }));
            }
        }
    }

    if issues.is_empty() {
        Ok(())
    } else {
        Err(WorkspaceError::ToolDetails {
            code: "EDIT_CONTRACT_INVALID",
            message: "Precise edit contract validation failed".into(),
            category: "validation",
            retryable: false,
            details: json!({
                "issue_count": issues.len(),
                "issues": issues,
                "suggestion": "Rebuild each edit using only the fields required by its type"
            }),
        })
    }
}

pub(super) fn apply_precise_edits(
    original: &str,
    edits: &[Value],
) -> Result<String, WorkspaceError> {
    let mut resolved = Vec::with_capacity(edits.len());
    for (index, edit) in edits.iter().enumerate() {
        resolved.extend(resolve_precise_edit(original, edit, index)?);
    }
    validate_resolved_edits(&resolved)?;

    resolved.sort_by(|left, right| {
        right
            .start_byte
            .cmp(&left.start_byte)
            .then_with(|| right.end_byte.cmp(&left.end_byte))
            .then_with(|| right.input_index.cmp(&left.input_index))
    });

    let mut content = original.to_string();
    for edit in resolved {
        content.replace_range(edit.start_byte..edit.end_byte, &edit.replacement);
    }
    Ok(content)
}

fn resolve_precise_edit(
    original: &str,
    edit: &Value,
    index: usize,
) -> Result<Vec<ResolvedEdit>, WorkspaceError> {
    let edit_type = edit.get("type").and_then(Value::as_str).ok_or_else(|| {
        WorkspaceError::invalid_argument(format!("edits[{index}].type is required"))
    })?;
    match edit_type {
        "replace" => {
            let old_text = required_edit_text(edit, index, "old_text")?;
            let replacement = adapt_newlines_to_original(
                edit.get("new_text").and_then(Value::as_str).unwrap_or(""),
                original,
            );
            let targets = resolve_text_targets(original, edit, old_text, index)?;
            Ok(targets
                .into_iter()
                .map(|(start_byte, end_byte)| ResolvedEdit {
                    input_index: index,
                    start_byte,
                    end_byte,
                    replacement: replacement.clone(),
                })
                .collect())
        }
        "insert_before" | "insert_after" => {
            let anchor = required_edit_text(edit, index, "anchor")?;
            let text =
                adapt_newlines_to_original(required_edit_text(edit, index, "text")?, original);
            let targets = resolve_text_targets(original, edit, anchor, index)?;
            Ok(targets
                .into_iter()
                .map(|(start, end)| {
                    let position = if edit_type == "insert_before" {
                        start
                    } else {
                        end
                    };
                    ResolvedEdit {
                        input_index: index,
                        start_byte: position,
                        end_byte: position,
                        replacement: text.clone(),
                    }
                })
                .collect())
        }
        "replace_lines" | "delete_lines" => {
            let start_line = required_line(edit, index, "start_line")?;
            let end_line = required_line(edit, index, "end_line")?;
            let (start_byte, end_byte) = line_range_bytes(original, start_line, end_line, index)?;
            if let Some(expected) = edit.get("expected_text").and_then(Value::as_str) {
                let actual = &original[start_byte..end_byte];
                if normalize_newlines(actual) != normalize_newlines(expected) {
                    return Err(WorkspaceError::ToolDetails {
                        code: "EDIT_EXPECTED_TEXT_MISMATCH",
                        message: format!(
                            "edits[{index}] line range content did not match expected_text"
                        ),
                        category: "conflict",
                        retryable: true,
                        details: json!({
                            "edit_index": index,
                            "start_line": start_line,
                            "end_line": end_line,
                            "actual_text": actual
                        }),
                    });
                }
            }
            Ok(vec![ResolvedEdit {
                input_index: index,
                start_byte,
                end_byte,
                replacement: if edit_type == "delete_lines" {
                    String::new()
                } else {
                    adapt_newlines_to_original(
                        edit.get("new_text").and_then(Value::as_str).unwrap_or(""),
                        original,
                    )
                },
            }])
        }
        other => Err(WorkspaceError::invalid_argument(format!(
            "Unsupported edits[{index}].type: {other}"
        ))),
    }
}

fn resolve_text_targets(
    original: &str,
    edit: &Value,
    target: &str,
    index: usize,
) -> Result<Vec<(usize, usize)>, WorkspaceError> {
    let before_context = edit.get("before_context").and_then(Value::as_str);
    let after_context = edit.get("after_context").and_then(Value::as_str);
    let start_line = edit
        .get("start_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let end_line = edit
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);

    let search_range = match (start_line, end_line) {
        (None, None) => (0, original.len()),
        (Some(start), Some(end)) => line_range_bytes(original, start, end, index)?,
        _ => {
            return Err(WorkspaceError::invalid_argument(format!(
                "edits[{index}].start_line and end_line must be provided together"
            )))
        }
    };

    let match_mode = edit
        .get("match_mode")
        .and_then(Value::as_str)
        .unwrap_or("exact");
    let candidates = match match_mode {
        "exact" => exact_text_candidates(original, target, search_range),
        "whitespace" => whitespace_text_candidates(original, target, search_range, index)?,
        other => {
            return Err(WorkspaceError::invalid_argument(format!(
                "edits[{index}].match_mode must be exact or whitespace, got {other}"
            )))
        }
    }
    .into_iter()
    .filter(|(start, end)| {
        context_matches(
            original,
            *start,
            *end,
            before_context,
            after_context,
            match_mode,
        )
    })
    .collect::<Vec<_>>();

    let expected = expected_occurrences(edit);
    if candidates.len() != expected {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_MATCH_COUNT_MISMATCH",
            message: format!(
                "edits[{index}] expected {expected} guarded matches but found {}",
                candidates.len()
            ),
            category: "validation",
            retryable: false,
            details: json!({
                "edit_index": index,
                "expected_occurrences": expected,
                "actual_occurrences": candidates.len(),
                "candidate_lines": candidates.iter().map(|(start, _)| byte_to_line(original, *start)).collect::<Vec<_>>(),
                "candidate_ranges": candidates.iter().map(|(start, end)| json!({
                    "start_line": byte_to_line(original, *start),
                    "end_line": byte_to_line(original, end.saturating_sub(1))
                })).collect::<Vec<_>>(),
                "candidate_contexts": text_candidate_contexts(original, &candidates, 3),
                "candidate_context_limit": 8,
                "candidate_contexts_truncated": candidates.len() > 8,
                "recovery_reason": if candidates.is_empty() {
                    "target_text_not_found"
                } else {
                    "target_text_not_unique"
                }
            }),
        });
    }
    Ok(candidates)
}

fn text_candidate_contexts(
    original: &str,
    candidates: &[(usize, usize)],
    radius: usize,
) -> Vec<Value> {
    let lines = original
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect::<Vec<_>>();
    candidates
        .iter()
        .take(8)
        .map(|(start, end)| {
            let match_start = byte_to_line(original, *start);
            let match_end = byte_to_line(original, end.saturating_sub(1));
            let context_start = match_start.saturating_sub(radius).max(1);
            let context_end = match_end.saturating_add(radius).min(lines.len());
            json!({
                "start_line": match_start,
                "end_line": match_end,
                "context_start_line": context_start,
                "context_end_line": context_end,
                "preview": lines[context_start - 1..context_end]
            })
        })
        .collect()
}

fn exact_text_candidates(
    original: &str,
    target: &str,
    search_range: (usize, usize),
) -> Vec<(usize, usize)> {
    let haystack = &original[search_range.0..search_range.1];
    if !target.contains('\n') {
        return haystack
            .match_indices(target)
            .map(|(offset, _)| {
                let start = search_range.0 + offset;
                (start, start + target.len())
            })
            .collect();
    }

    let normalized_target = normalize_newlines(target);
    let (normalized_haystack, original_boundaries) = normalize_newlines_with_boundary_map(haystack);
    normalized_haystack
        .match_indices(&normalized_target)
        .map(|(normalized_start, matched)| {
            let normalized_end = normalized_start + matched.len();
            (
                search_range.0 + original_boundaries[normalized_start],
                search_range.0 + original_boundaries[normalized_end],
            )
        })
        .collect()
}

fn normalize_newlines_with_boundary_map(value: &str) -> (String, Vec<usize>) {
    let bytes = value.as_bytes();
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut original_boundaries = Vec::with_capacity(bytes.len() + 1);
    original_boundaries.push(0);

    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            index += 2;
        } else {
            normalized.push(bytes[index]);
            index += 1;
        }
        original_boundaries.push(index);
    }

    (
        String::from_utf8(normalized).expect("normalizing CRLF preserves valid UTF-8"),
        original_boundaries,
    )
}

pub(super) fn whitespace_text_candidates(
    original: &str,
    target: &str,
    search_range: (usize, usize),
    edit_index: usize,
) -> Result<Vec<(usize, usize)>, WorkspaceError> {
    let pattern = whitespace_flexible_pattern(target);
    let regex = Regex::new(&pattern).map_err(|error| {
        WorkspaceError::invalid_argument(format!(
            "edits[{edit_index}] could not build whitespace matcher: {error}"
        ))
    })?;
    Ok(regex
        .find_iter(&original[search_range.0..search_range.1])
        .map(|matched| {
            (
                search_range.0 + matched.start(),
                search_range.0 + matched.end(),
            )
        })
        .collect())
}

fn whitespace_flexible_pattern(target: &str) -> String {
    let mut pattern = String::new();
    let mut literal = String::new();
    let mut in_whitespace = false;
    for character in target.chars() {
        if character.is_whitespace() {
            if !literal.is_empty() {
                pattern.push_str(&regex::escape(&literal));
                literal.clear();
            }
            if !in_whitespace {
                pattern.push_str(r"\s+");
                in_whitespace = true;
            }
        } else {
            literal.push(character);
            in_whitespace = false;
        }
    }
    if !literal.is_empty() {
        pattern.push_str(&regex::escape(&literal));
    }
    pattern
}

fn context_matches(
    original: &str,
    start: usize,
    end: usize,
    before_context: Option<&str>,
    after_context: Option<&str>,
    match_mode: &str,
) -> bool {
    let before_matches = before_context.is_none_or(|before| match match_mode {
        "whitespace" => flexible_suffix_matches(&original[..start], before),
        _ => newline_flexible_suffix_matches(&original[..start], before),
    });
    let after_matches = after_context.is_none_or(|after| match match_mode {
        "whitespace" => flexible_prefix_matches(&original[end..], after),
        _ => newline_flexible_prefix_matches(&original[end..], after),
    });
    before_matches && after_matches
}

fn newline_flexible_suffix_matches(haystack: &str, expected: &str) -> bool {
    if !expected.contains('\n') {
        return haystack.ends_with(expected);
    }

    let normalized_expected = normalize_newlines(expected);
    let haystack = haystack.as_bytes();
    let expected = normalized_expected.as_bytes();
    let mut haystack_index = haystack.len();
    let mut expected_index = expected.len();

    while expected_index > 0 {
        let expected_byte = expected[expected_index - 1];
        if expected_byte == b'\n' {
            if haystack_index >= 2
                && haystack[haystack_index - 2] == b'\r'
                && haystack[haystack_index - 1] == b'\n'
            {
                haystack_index -= 2;
            } else if haystack_index >= 1 && haystack[haystack_index - 1] == b'\n' {
                haystack_index -= 1;
            } else {
                return false;
            }
        } else if haystack_index >= 1 && haystack[haystack_index - 1] == expected_byte {
            haystack_index -= 1;
        } else {
            return false;
        }
        expected_index -= 1;
    }

    true
}

fn newline_flexible_prefix_matches(haystack: &str, expected: &str) -> bool {
    if !expected.contains('\n') {
        return haystack.starts_with(expected);
    }

    let normalized_expected = normalize_newlines(expected);
    let haystack = haystack.as_bytes();
    let expected = normalized_expected.as_bytes();
    let mut haystack_index = 0;
    let mut expected_index = 0;

    while expected_index < expected.len() {
        let expected_byte = expected[expected_index];
        if expected_byte == b'\n' {
            if haystack.get(haystack_index) == Some(&b'\r')
                && haystack.get(haystack_index + 1) == Some(&b'\n')
            {
                haystack_index += 2;
            } else if haystack.get(haystack_index) == Some(&b'\n') {
                haystack_index += 1;
            } else {
                return false;
            }
        } else if haystack.get(haystack_index) == Some(&expected_byte) {
            haystack_index += 1;
        } else {
            return false;
        }
        expected_index += 1;
    }

    true
}

fn flexible_suffix_matches(haystack: &str, expected: &str) -> bool {
    Regex::new(&format!(r"(?:{})$", whitespace_flexible_pattern(expected)))
        .is_ok_and(|regex| regex.is_match(haystack))
}

fn flexible_prefix_matches(haystack: &str, expected: &str) -> bool {
    Regex::new(&format!(r"^(?:{})", whitespace_flexible_pattern(expected)))
        .is_ok_and(|regex| regex.is_match(haystack))
}

fn validate_resolved_edits(edits: &[ResolvedEdit]) -> Result<(), WorkspaceError> {
    for (i, left) in edits.iter().enumerate() {
        for right in edits.iter().skip(i + 1) {
            let overlap = left.start_byte < right.end_byte && right.start_byte < left.end_byte;
            let insertion_inside = (left.start_byte == left.end_byte
                && left.start_byte > right.start_byte
                && left.start_byte < right.end_byte)
                || (right.start_byte == right.end_byte
                    && right.start_byte > left.start_byte
                    && right.start_byte < left.end_byte);
            if overlap || insertion_inside {
                return Err(WorkspaceError::ToolDetails {
                    code: "EDIT_RANGES_OVERLAP",
                    message: format!(
                        "edits[{}] overlaps edits[{}] on the original file",
                        left.input_index, right.input_index
                    ),
                    category: "validation",
                    retryable: false,
                    details: json!({
                        "first_edit_index": left.input_index,
                        "second_edit_index": right.input_index,
                        "first_range": [left.start_byte, left.end_byte],
                        "second_range": [right.start_byte, right.end_byte]
                    }),
                });
            }
        }
    }
    Ok(())
}

pub(super) fn required_edit_text<'a>(
    edit: &'a Value,
    index: usize,
    key: &str,
) -> Result<&'a str, WorkspaceError> {
    let value = edit.get(key).and_then(Value::as_str).ok_or_else(|| {
        WorkspaceError::invalid_argument(format!("edits[{index}].{key} is required"))
    })?;
    if value.is_empty() {
        return Err(WorkspaceError::invalid_argument(format!(
            "edits[{index}].{key} must not be empty"
        )));
    }
    Ok(value)
}

fn required_line(edit: &Value, index: usize, key: &str) -> Result<usize, WorkspaceError> {
    edit.get(key)
        .and_then(Value::as_u64)
        .filter(|line| *line > 0)
        .map(|line| line as usize)
        .ok_or_else(|| {
            WorkspaceError::invalid_argument(format!(
                "edits[{index}].{key} must be a positive integer"
            ))
        })
}

pub(super) fn expected_occurrences(edit: &Value) -> usize {
    edit.get("expected_occurrences")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize
}

fn normalize_newlines(value: &str) -> String {
    value.replace("\r\n", "\n")
}

fn preferred_line_ending(value: &str) -> &'static str {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'\n' {
            return if index > 0 && bytes[index - 1] == b'\r' {
                "\r\n"
            } else {
                "\n"
            };
        }
    }
    "\n"
}

pub(super) fn adapt_newlines_to_original(value: &str, original: &str) -> String {
    let normalized = normalize_newlines(value);
    if preferred_line_ending(original) == "\r\n" {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    }
}

pub(super) fn line_range_bytes(
    content: &str,
    start_line: usize,
    end_line: usize,
    edit_index: usize,
) -> Result<(usize, usize), WorkspaceError> {
    let mut starts = vec![0usize];
    for (index, byte) in content.bytes().enumerate() {
        if byte == b'\n' && index + 1 < content.len() {
            starts.push(index + 1);
        }
    }
    let total_lines = starts.len();
    if start_line == 0 || start_line > end_line || end_line > total_lines {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_LINE_RANGE_INVALID",
            message: format!(
                "edits[{edit_index}] line range {start_line}-{end_line} is outside 1-{total_lines}"
            ),
            category: "validation",
            retryable: false,
            details: json!({
                "edit_index": edit_index,
                "start_line": start_line,
                "end_line": end_line,
                "total_lines": total_lines
            }),
        });
    }
    let start = starts[start_line - 1];
    let end = if end_line < total_lines {
        starts[end_line]
    } else {
        content.len()
    };
    Ok((start, end))
}

pub(super) fn byte_to_line(content: &str, byte: usize) -> usize {
    content[..byte]
        .bytes()
        .filter(|value| *value == b'\n')
        .count()
        + 1
}
