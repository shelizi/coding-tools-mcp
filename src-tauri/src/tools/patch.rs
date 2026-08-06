use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use uuid::Uuid;

use crate::tools::context::ToolContext;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};

const EDIT_PROPOSAL_TTL: Duration = Duration::from_secs(300);
const MAX_EDIT_PROPOSALS: usize = 200;
const MAX_PROPOSAL_PATCH_BYTES: usize = 64 * 1024;
const MAX_PROPOSAL_REPLACEMENT_BYTES: usize = 128 * 1024;
const MAX_PROPOSAL_PREVIEW_BYTES: usize = 128 * 1024;
const SMALL_PROPOSAL_REPLACEMENT_BYTES: usize = 8 * 1024;
const PATCH_EFFICIENCY_PERCENT: usize = 80;

#[derive(Debug, Clone)]
struct EditProposal {
    path: String,
    file_sha256: String,
    start_byte: usize,
    end_byte: usize,
    actual_text: String,
    replacement: String,
    created_at: SystemTime,
}

static EDIT_PROPOSALS: OnceLock<Mutex<HashMap<String, EditProposal>>> = OnceLock::new();

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let ws = &ctx.workspace;
    let patch = args
        .get("patch")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("patch is required"))?;
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let confirm = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let file_patches = parse_unified_diff(patch)?;
    if file_patches.is_empty() {
        return Err(patch_failed("No files were modified."));
    }
    if let Some(path) = file_patches
        .iter()
        .find(|file| is_protected_repository_asset(&file.path))
        .map(|file| file.path.as_str())
    {
        return Err(protected_repository_asset(format!(
            "禁止删除仓库保护资产: {path}"
        )));
    }
    if !confirm {
        if let Some(path) = file_patches
            .iter()
            .find(|file| file.is_deleted && is_critical_file(&file.path))
            .map(|file| file.path.as_str())
        {
            return Err(dangerous_operation(format!(
                "删除关键项目文件需要 confirm=true: {path}"
            )));
        }
    }

    let mut affected = Vec::new();
    let mut summaries = Vec::new();
    let mut staged: HashMap<String, Option<String>> = HashMap::new();
    let mut preflight_versions: HashMap<String, Option<String>> = HashMap::new();
    let mut file_versions = Vec::new();
    let mut diff = String::new();

    for fp in &file_patches {
        ws.reject_unsafe_text(&fp.path)?;
        ws.reject_write_symlink(&fp.path)?;
        let resolved = if fp.is_new_file {
            ws.resolve_for_write(&fp.path)?
        } else {
            ws.resolve_existing(&fp.path)?
        };

        let actual_bytes =
            if resolved.existed {
                Some(fs::read(&resolved.path).map_err(|_| {
                    WorkspaceError::not_found(format!("File not found: {}", fp.path))
                })?)
            } else {
                None
            };
        let before_sha256 = actual_bytes.as_deref().map(sha256_hex);
        if let Some(expected) = expected_hash_for(args, &fp.path) {
            match before_sha256.as_deref() {
                Some(actual) if expected.eq_ignore_ascii_case(actual) => {}
                Some(actual) => return Err(version_mismatch(&fp.path, expected, actual)),
                None => {
                    return Err(version_mismatch(&fp.path, expected, "missing"));
                }
            }
        }

        let replacing_after_delete = staged.get(&resolved.display).is_some_and(Option::is_none);
        if fp.is_new_file && resolved.existed && !replacing_after_delete {
            return Err(WorkspaceError::ToolDetails {
                code: "FILE_ALREADY_EXISTS",
                message: format!("Add File target already exists: {}", fp.path),
                category: "conflict",
                retryable: false,
                details: json!({
                    "path": fp.path,
                    "actual_sha256": before_sha256
                }),
            });
        }

        let original = if fp.is_new_file {
            String::new()
        } else {
            let bytes = actual_bytes
                .as_deref()
                .ok_or_else(|| WorkspaceError::not_found(format!("File not found: {}", fp.path)))?;
            String::from_utf8(bytes.to_vec()).map_err(|_| WorkspaceError::Tool {
                code: "UNSUPPORTED_ENCODING",
                message: format!("File is not valid utf-8: {}", fp.path),
                category: "validation",
                retryable: false,
            })?
        };

        preflight_versions
            .entry(resolved.display.clone())
            .or_insert_with(|| before_sha256.clone());

        if fp.is_deleted {
            diff.push_str(&unified_diff(&resolved.display, &original, "", false, true));
            staged.insert(resolved.display.clone(), None);
            affected.push(json!({ "path": resolved.display, "operation": "delete" }));
            summaries.push(format!("D {}", resolved.display));
            file_versions.push(json!({
                "path": resolved.display,
                "before_sha256": before_sha256,
                "after_sha256": Value::Null
            }));
            continue;
        }

        let updated = apply_hunks(&original, &fp.hunks)?;
        if updated == original {
            return Err(patch_failed(format!(
                "Patch produced no changes for {}",
                fp.path
            )));
        }
        let op = if fp.is_new_file && !replacing_after_delete {
            "add"
        } else {
            "update"
        };
        let after_sha256 = sha256_hex(updated.as_bytes());
        diff.push_str(&unified_diff(
            &resolved.display,
            &original,
            &updated,
            fp.is_new_file,
            false,
        ));
        staged.insert(resolved.display.clone(), Some(updated));
        affected.push(json!({ "path": resolved.display, "operation": op }));
        summaries.push(format!(
            "{} {}",
            if op == "add" { "A" } else { "M" },
            resolved.display
        ));
        file_versions.push(json!({
            "path": resolved.display,
            "before_sha256": before_sha256,
            "after_sha256": after_sha256
        }));
    }

    let files_created = affected_paths(&affected, "add");
    let files_modified = affected_paths(&affected, "update");
    let files_deleted = affected_paths(&affected, "delete");

    if !dry_run {
        for (path, expected) in &preflight_versions {
            verify_file_version(ws, path, expected.as_deref())?;
        }
        let _transaction_backups = commit_staged(ws, &staged)?;
        let change_id = Uuid::new_v4().simple().to_string();
        return Ok(tool_ok(json!({
            "dry_run": false,
            "preflight": true,
            "applied": true,
            "clean": true,
            "change_id": change_id,
            "summary": summaries.join("\n"),
            "diff": diff,
            "file_versions": file_versions,
            "affected_files": affected,
            "files_created": files_created,
            "files_modified": files_modified,
            "files_deleted": files_deleted,
            "recovery": "git",
            "warnings": []
        })));
    }

    Ok(tool_ok(json!({
        "dry_run": true,
        "preflight": true,
        "applied": false,
        "clean": true,
        "summary": summaries.join("\n"),
        "diff": diff,
        "file_versions": file_versions,
        "affected_files": affected,
        "would_create": files_created,
        "would_modify": files_modified,
        "would_delete": files_deleted,
        "warnings": []
    })))
}

pub fn patch_check(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let mut check_args = args.clone();
    check_args["dry_run"] = Value::Bool(true);
    let mut result = apply_patch(ctx, &check_args)?;
    if let Some(object) = result.as_object_mut() {
        object.insert("preflight".into(), Value::Bool(true));
    }
    Ok(result)
}

pub fn edit_file(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let started = Instant::now();
    let ws = &ctx.workspace;
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    if args.get("edits").is_some() && args.get("apply_proposal").is_some() {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_CONTRACT_INVALID",
            message: "edit_file accepts either edits or apply_proposal, not both".into(),
            category: "validation",
            retryable: false,
            details: json!({
                "path": path,
                "issue_count": 1,
                "issues": [{
                    "field": "edits,apply_proposal",
                    "reason": "mutually_exclusive_fields"
                }],
                "suggestion": "Send precise edits or apply one stored proposal",
                "recovery_actions": [{
                    "action": "choose_edit_mode",
                    "tool": "edit",
                    "arguments": { "files": [{ "path": path }] },
                    "required_arguments": ["files[0].edits_or_apply_proposal"],
                    "reason": "mutually_exclusive_fields"
                }]
            }),
        });
    }
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    ws.reject_unsafe_text(path)?;
    ws.reject_write_symlink(path)?;
    let resolved = ws.resolve_existing(path)?;
    if !resolved.path.is_file() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "edit_file target must be a file".into(),
            category: "validation",
            retryable: false,
        });
    }
    let original_bytes = fs::read(&resolved.path)
        .map_err(|_| WorkspaceError::not_found(format!("File not found: {path}")))?;
    if original_bytes.iter().take(4096).any(|byte| *byte == 0) {
        return Err(WorkspaceError::Tool {
            code: "BINARY_FILE",
            message: "Binary file edit blocked for text tool.".into(),
            category: "validation",
            retryable: false,
        });
    }
    let before_sha256 = sha256_hex(&original_bytes);
    if let Some(expected) = args.get("expected_sha256").and_then(Value::as_str) {
        if !expected.eq_ignore_ascii_case(&before_sha256) {
            return Err(version_mismatch(
                &resolved.display,
                expected,
                &before_sha256,
            ));
        }
    }
    let original = String::from_utf8(original_bytes).map_err(|_| WorkspaceError::Tool {
        code: "UNSUPPORTED_ENCODING",
        message: "File is not valid utf-8.".into(),
        category: "validation",
        retryable: false,
    })?;

    let (updated, proposal_id, proposal_apply_format) =
        if let Some(apply) = args.get("apply_proposal") {
            apply_edit_proposal(&resolved.display, &before_sha256, &original, apply)?
        } else {
            let edits = args.get("edits").and_then(Value::as_array).ok_or_else(|| {
                WorkspaceError::invalid_argument("edits or apply_proposal is required")
            })?;
            if edits.is_empty() {
                return Err(WorkspaceError::invalid_argument("edits must not be empty"));
            }
            if let Err(error) = validate_precise_edit_contract(edits) {
                return Err(enrich_edit_error(error, &resolved.display, &before_sha256));
            }
            match apply_precise_edits(&original, edits) {
                Ok(updated) => (updated, None, "direct"),
                Err(error) => {
                    if let Some(proposal) =
                        build_edit_proposal(&resolved.display, &before_sha256, &original, edits)?
                    {
                        return Ok(tool_ok(proposal));
                    }
                    return Err(enrich_edit_error(error, &resolved.display, &before_sha256));
                }
            }
        };

    if updated == original {
        return Err(patch_failed("Edits produced no changes."));
    }
    let after_sha256 = sha256_hex(updated.as_bytes());
    let diff = unified_diff(&resolved.display, &original, &updated, false, false);
    let change_id = if dry_run {
        Value::Null
    } else {
        Value::String(Uuid::new_v4().simple().to_string())
    };
    let preflight_finished = Instant::now();
    let edit_plan = if dry_run {
        let mut replay_file = serde_json::Map::new();
        replay_file.insert("path".into(), json!(resolved.display));
        replay_file.insert("expected_sha256".into(), json!(before_sha256));
        if let Some(edits) = args.get("edits") {
            replay_file.insert("edits".into(), edits.clone());
        }
        if let Some(apply_proposal) = args.get("apply_proposal") {
            replay_file.insert("apply_proposal".into(), apply_proposal.clone());
        }
        let mut replay_arguments = serde_json::Map::new();
        replay_arguments.insert(
            "files".into(),
            Value::Array(vec![Value::Object(replay_file)]),
        );
        replay_arguments.insert("dry_run".into(), Value::Bool(false));
        if let Some(reason) = replay_reason(args) {
            replay_arguments.insert("reason".into(), reason);
        }
        let stateful_dependencies = proposal_id.as_ref().map_or_else(
            || json!([]),
            |proposal_id| {
                json!([{
                    "type": "edit_proposal",
                    "proposal_id": proposal_id,
                    "ttl_seconds": EDIT_PROPOSAL_TTL.as_secs()
                }])
            },
        );
        replayable_edit_plan(
            "edit",
            Value::Object(replay_arguments),
            json!([{
                "path": resolved.display,
                "before_sha256": before_sha256,
                "after_sha256": after_sha256
            }]),
            stateful_dependencies,
        )
    } else {
        Value::Null
    };
    let plan_finished = Instant::now();
    if !dry_run {
        verify_file_version(ws, &resolved.display, Some(&before_sha256))?;
        let mut staged = HashMap::new();
        staged.insert(resolved.display.clone(), Some(updated));
        let _transaction_backups = commit_staged(ws, &staged)?;
        if let Some(proposal_id) = proposal_id.as_deref() {
            proposal_store()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .remove(proposal_id);
        }
    }

    let completed = Instant::now();
    let phase_durations_ms = json!({
        "preflight_ms": preflight_finished.duration_since(started).as_millis(),
        "plan_ms": plan_finished.duration_since(preflight_finished).as_millis(),
        "commit_ms": completed.duration_since(plan_finished).as_millis(),
        "total_ms": completed.duration_since(started).as_millis()
    });
    Ok(tool_ok(json!({
        "status": if proposal_id.is_some() { "proposal_applied" } else { "edited" },
        "proposal_id": proposal_id,
        "proposal_apply_format": proposal_apply_format,
        "dry_run": dry_run,
        "preflight": true,
        "applied": !dry_run,
        "clean": true,
        "change_id": change_id,
        "path": resolved.display,
        "operation": "update",
        "before_sha256": before_sha256,
        "after_sha256": after_sha256,
        "edit_plan": edit_plan,
        "diff": diff,
        "phase_durations_ms": phase_durations_ms,
        "affected_files": [{ "path": resolved.display, "operation": "update" }],
        "files_created": Vec::<String>::new(),
        "files_modified": [resolved.display],
        "files_deleted": Vec::<String>::new(),
        "recovery": if dry_run { Value::Null } else { json!("git") },
        "warnings": []
    })))
}

fn proposal_store() -> &'static Mutex<HashMap<String, EditProposal>> {
    EDIT_PROPOSALS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn prune_edit_proposals(proposals: &mut HashMap<String, EditProposal>) {
    proposals.retain(|_, proposal| {
        proposal
            .created_at
            .elapsed()
            .map(|age| age <= EDIT_PROPOSAL_TTL)
            .unwrap_or(false)
    });
    while proposals.len() >= MAX_EDIT_PROPOSALS {
        let Some(oldest) = proposals
            .iter()
            .min_by_key(|(_, proposal)| proposal.created_at)
            .map(|(id, _)| id.clone())
        else {
            break;
        };
        proposals.remove(&oldest);
    }
}

fn build_edit_proposal(
    path: &str,
    file_sha256: &str,
    original: &str,
    edits: &[Value],
) -> Result<Option<Value>, WorkspaceError> {
    if edits.len() != 1 {
        return Ok(None);
    }
    let edit = &edits[0];
    if edit.get("type").and_then(Value::as_str) != Some("replace")
        || edit
            .get("match_mode")
            .and_then(Value::as_str)
            .unwrap_or("exact")
            != "exact"
        || expected_occurrences(edit) != 1
    {
        return Ok(None);
    }
    let old_text = required_edit_text(edit, 0, "old_text")?;
    let requested_replacement = edit.get("new_text").and_then(Value::as_str).unwrap_or("");
    let replacement = adapt_newlines_to_original(requested_replacement, original);
    let search_range = match (
        edit.get("start_line").and_then(Value::as_u64),
        edit.get("end_line").and_then(Value::as_u64),
    ) {
        (None, None) => (0, original.len()),
        (Some(start), Some(end)) => line_range_bytes(original, start as usize, end as usize, 0)?,
        _ => return Ok(None),
    };
    let candidates = whitespace_text_candidates(original, old_text, search_range, 0)?;
    if candidates.len() != 1 {
        return Ok(None);
    }
    let (start_byte, end_byte) = candidates[0];
    let actual_text = original[start_byte..end_byte].to_string();
    let proposal_id = Uuid::new_v4().simple().to_string();
    let proposal = EditProposal {
        path: path.to_string(),
        file_sha256: file_sha256.to_string(),
        start_byte,
        end_byte,
        actual_text: actual_text.clone(),
        replacement: replacement.clone(),
        created_at: SystemTime::now(),
    };
    let mut proposed_content = original.to_string();
    proposed_content.replace_range(start_byte..end_byte, &replacement);
    let proposed_content_bytes = proposed_content.len();
    let proposed_content_sha256 = sha256_hex(proposed_content.as_bytes());
    let proposed_content_included = proposed_content_bytes <= MAX_PROPOSAL_PREVIEW_BYTES;
    let replacement_bytes = replacement.len();
    let preferred_format = if replacement_bytes <= SMALL_PROPOSAL_REPLACEMENT_BYTES {
        "replacement"
    } else {
        "patch"
    };
    let preferred_format_reason = if preferred_format == "replacement" {
        "small_replacement_is_cheaper"
    } else {
        "large_replacement_may_benefit_from_patch"
    };
    let mut proposals = proposal_store()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    prune_edit_proposals(&mut proposals);
    proposals.insert(proposal_id.clone(), proposal);

    Ok(Some(json!({
        "status": "proposal_required",
        "applied": false,
        "proposal_id": proposal_id,
        "proposal_ttl_seconds": EDIT_PROPOSAL_TTL.as_secs(),
        "path": path,
        "file_sha256": file_sha256,
        "candidate_start_line": byte_to_line(original, start_byte),
        "candidate_end_line": byte_to_line(original, end_byte.saturating_sub(1)),
        "actual_text": actual_text,
        "requested_old_text": old_text,
        "requested_new_text": requested_replacement,
        "candidate_diff": unified_diff(path, old_text, &original[start_byte..end_byte], false, false),
        "proposed_content": if proposed_content_included { json!(proposed_content) } else { Value::Null },
        "proposed_content_bytes": proposed_content_bytes,
        "proposed_content_included": proposed_content_included,
        "proposed_content_sha256": proposed_content_sha256,
        "accepted_formats": ["accept", "replacement", "patch"],
        "preferred_format": preferred_format,
        "preferred_format_reason": preferred_format_reason,
        "replacement_bytes": replacement_bytes,
        "replacement_max_bytes": MAX_PROPOSAL_REPLACEMENT_BYTES,
        "small_replacement_threshold_bytes": SMALL_PROPOSAL_REPLACEMENT_BYTES,
        "patch_efficiency_percent": PATCH_EFFICIENCY_PERCENT,
        "proposal_patch_format": "unified_diff_single_file_single_hunk",
        "proposal_patch_max_bytes": MAX_PROPOSAL_PATCH_BYTES,
        "next_action": "apply_proposal",
        "warnings": []
    })))
}

fn apply_edit_proposal(
    path: &str,
    file_sha256: &str,
    original: &str,
    apply: &Value,
) -> Result<(String, Option<String>, &'static str), WorkspaceError> {
    let proposal_id = apply
        .get("proposal_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            WorkspaceError::invalid_argument("apply_proposal.proposal_id is required")
        })?;
    let proposal_patch = apply.get("patch").and_then(Value::as_str);
    let proposal_replacement = apply.get("replacement").and_then(Value::as_str);
    if proposal_patch.is_some() && proposal_replacement.is_some() {
        return Err(WorkspaceError::invalid_argument(
            "apply_proposal.patch and apply_proposal.replacement are mutually exclusive",
        ));
    }
    if proposal_patch.is_some_and(|patch| patch.len() > MAX_PROPOSAL_PATCH_BYTES) {
        return Err(WorkspaceError::invalid_argument(format!(
            "apply_proposal.patch exceeds {MAX_PROPOSAL_PATCH_BYTES} bytes"
        )));
    }
    if proposal_replacement
        .is_some_and(|replacement| replacement.len() > MAX_PROPOSAL_REPLACEMENT_BYTES)
    {
        return Err(WorkspaceError::invalid_argument(format!(
            "apply_proposal.replacement exceeds {MAX_PROPOSAL_REPLACEMENT_BYTES} bytes"
        )));
    }

    let proposal = {
        let mut proposals = proposal_store()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        prune_edit_proposals(&mut proposals);
        proposals
            .get(proposal_id)
            .cloned()
            .ok_or_else(|| WorkspaceError::ToolDetails {
                code: "EDIT_PROPOSAL_NOT_FOUND",
                message: "Edit proposal was not found or has expired.".into(),
                category: "conflict",
                retryable: true,
                details: json!({"proposal_id": proposal_id, "reason": "missing_or_expired"}),
            })?
    };

    if proposal.path != path || proposal.file_sha256 != file_sha256 {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_PROPOSAL_STALE",
            message: "Edit proposal no longer matches the current file.".into(),
            category: "conflict",
            retryable: true,
            details: json!({"proposal_id": proposal_id, "reason": "file_changed"}),
        });
    }
    if original.get(proposal.start_byte..proposal.end_byte) != Some(proposal.actual_text.as_str()) {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_PROPOSAL_STALE",
            message: "Edit proposal candidate no longer matches the current file.".into(),
            category: "conflict",
            retryable: true,
            details: json!({"proposal_id": proposal_id, "reason": "candidate_changed"}),
        });
    }

    let (replacement, apply_format) = if let Some(patch) = proposal_patch {
        let patched = apply_restricted_proposal_patch(&proposal.replacement, patch)?;
        if patch.len().saturating_mul(100) >= patched.len().saturating_mul(PATCH_EFFICIENCY_PERCENT)
        {
            return Err(WorkspaceError::ToolDetails {
                code: "EDIT_PROPOSAL_PATCH_INEFFICIENT",
                message:
                    "Proposal patch costs as much as or more than sending the full replacement."
                        .into(),
                category: "validation",
                retryable: true,
                details: json!({
                    "reason": "replacement_is_cheaper",
                    "patch_bytes": patch.len(),
                    "replacement_bytes": patched.len(),
                    "patch_efficiency_percent": PATCH_EFFICIENCY_PERCENT,
                    "recommended_format": "replacement",
                    "recommended_replacement": patched
                }),
            });
        }
        (patched, "patch")
    } else if let Some(replacement) = proposal_replacement {
        (replacement.to_string(), "replacement")
    } else {
        (proposal.replacement.clone(), "accept")
    };

    let replacement = adapt_newlines_to_original(&replacement, original);
    let mut updated = original.to_string();
    updated.replace_range(proposal.start_byte..proposal.end_byte, &replacement);
    Ok((updated, Some(proposal_id.to_string()), apply_format))
}

fn apply_restricted_proposal_patch(
    proposed_text: &str,
    patch: &str,
) -> Result<String, WorkspaceError> {
    let files = parse_unified_diff(patch).map_err(|error| WorkspaceError::ToolDetails {
        code: "EDIT_PROPOSAL_PATCH_INVALID",
        message: "Proposal patch is not a valid unified diff.".into(),
        category: "validation",
        retryable: true,
        details: json!({
            "reason": "invalid_unified_diff",
            "source_error": error.to_error_value()
        }),
    })?;
    if files.len() != 1 || files[0].hunks.len() != 1 || files[0].is_new_file || files[0].is_deleted
    {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_PROPOSAL_PATCH_INVALID",
            message: "Proposal patch must contain exactly one file and one update hunk.".into(),
            category: "validation",
            retryable: true,
            details: json!({
                "reason": "single_file_single_hunk_required",
                "file_count": files.len(),
                "hunk_count": files.first().map(|file| file.hunks.len()).unwrap_or(0)
            }),
        });
    }
    let updated = apply_hunks(proposed_text, &files[0].hunks).map_err(|error| {
        WorkspaceError::ToolDetails {
            code: "EDIT_PROPOSAL_PATCH_MISMATCH",
            message: "Proposal patch did not apply exactly to the proposed replacement.".into(),
            category: "conflict",
            retryable: true,
            details: json!({
                "reason": "proposal_text_mismatch",
                "source_error": error.to_error_value()
            }),
        }
    })?;
    if updated == proposed_text {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_PROPOSAL_PATCH_NO_CHANGES",
            message: "Proposal patch produced no changes.".into(),
            category: "validation",
            retryable: true,
            details: json!({"reason": "no_changes"}),
        });
    }
    Ok(updated)
}

pub fn edit(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let files = args
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkspaceError::invalid_argument("files is required"))?;
    if files.is_empty() {
        return Err(WorkspaceError::invalid_argument("files must not be empty"));
    }
    if files.len() > 100 {
        return Err(WorkspaceError::invalid_argument(
            "edit supports at most 100 files",
        ));
    }
    if files.len() == 1 {
        let file = files[0]
            .as_object()
            .ok_or_else(|| WorkspaceError::invalid_argument("files[0] must be an object"))?;
        let mut single = serde_json::Map::new();
        for field in ["path", "expected_sha256", "edits", "apply_proposal"] {
            if let Some(value) = file.get(field) {
                single.insert(field.to_string(), value.clone());
            }
        }
        for field in ["dry_run", "reason"] {
            if let Some(value) = args.get(field) {
                single.insert(field.to_string(), value.clone());
            }
        }
        let mut result = edit_file(ctx, &Value::Object(single))?;
        if let Some(object) = result.as_object_mut() {
            object.insert("atomic".into(), Value::Bool(true));
        }
        return Ok(result);
    }
    if files
        .iter()
        .any(|file| file.get("apply_proposal").is_some())
    {
        return Err(WorkspaceError::ToolDetails {
            code: "EDIT_CONTRACT_INVALID",
            message: "apply_proposal is supported only when edit contains one file".into(),
            category: "validation",
            retryable: false,
            details: json!({
                "file_count": files.len(),
                "suggestion": "Apply a proposal in a single-file edit call",
                "recovery_actions": [{
                    "action": "split_proposal_edit",
                    "tool": "edit",
                    "required_arguments": ["files"],
                    "reason": "proposal_requires_single_file"
                }]
            }),
        });
    }
    edit_many(ctx, args)
}

pub fn edit_many(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let started = Instant::now();
    let files = args
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkspaceError::invalid_argument("files is required"))?;
    if files.is_empty() {
        return Err(WorkspaceError::invalid_argument("files must not be empty"));
    }
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ws = &ctx.workspace;
    let mut staged = HashMap::new();
    let mut versions = HashMap::new();
    let mut file_versions = Vec::new();
    let mut affected = Vec::new();
    let mut diffs = String::new();
    let mut seen = HashSet::new();

    for (file_index, file) in files.iter().enumerate() {
        let path = file
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| WorkspaceError::invalid_argument("files[].path is required"))?;
        if !seen.insert(path.to_string()) {
            return Err(WorkspaceError::invalid_argument(format!(
                "duplicate edit_many path: {path}"
            )));
        }
        let edits = file
            .get("edits")
            .and_then(Value::as_array)
            .ok_or_else(|| WorkspaceError::invalid_argument("files[].edits is required"))?;
        if edits.is_empty() {
            return Err(WorkspaceError::invalid_argument(
                "files[].edits must not be empty",
            ));
        }
        ws.reject_unsafe_text(path)?;
        ws.reject_write_symlink(path)?;
        let resolved = ws.resolve_existing(path)?;
        if !resolved.path.is_file() {
            return Err(WorkspaceError::invalid_argument(format!(
                "edit_many target must be a file: {path}"
            )));
        }
        let original_bytes = fs::read(&resolved.path)
            .map_err(|_| WorkspaceError::not_found(format!("File not found: {path}")))?;
        if original_bytes.iter().take(4096).any(|byte| *byte == 0) {
            return Err(WorkspaceError::Tool {
                code: "BINARY_FILE",
                message: format!("Binary file edit blocked: {path}"),
                category: "validation",
                retryable: false,
            });
        }
        let before_sha256 = sha256_hex(&original_bytes);
        if let Some(expected) = file.get("expected_sha256").and_then(Value::as_str) {
            if !expected.eq_ignore_ascii_case(&before_sha256) {
                return Err(enrich_edit_many_error(
                    version_mismatch(&resolved.display, expected, &before_sha256),
                    &resolved.display,
                    &before_sha256,
                    file_index,
                ));
            }
        }
        let original = String::from_utf8(original_bytes).map_err(|_| WorkspaceError::Tool {
            code: "UNSUPPORTED_ENCODING",
            message: format!("File is not valid UTF-8: {path}"),
            category: "validation",
            retryable: false,
        })?;
        if let Err(error) = validate_precise_edit_contract(edits) {
            return Err(enrich_edit_many_error(
                error,
                &resolved.display,
                &before_sha256,
                file_index,
            ));
        }
        let updated = apply_precise_edits(&original, edits).map_err(|error| {
            enrich_edit_many_error(error, &resolved.display, &before_sha256, file_index)
        })?;
        if updated == original {
            return Err(patch_failed(format!("Edits produced no changes: {path}")));
        }
        let after_sha256 = sha256_hex(updated.as_bytes());
        diffs.push_str(&unified_diff(
            &resolved.display,
            &original,
            &updated,
            false,
            false,
        ));
        versions.insert(resolved.display.clone(), Some(before_sha256.clone()));
        staged.insert(resolved.display.clone(), Some(updated));
        file_versions.push(json!({
            "path": resolved.display,
            "before_sha256": before_sha256,
            "after_sha256": after_sha256
        }));
        affected.push(json!({"path": resolved.display, "operation": "update"}));
    }

    let preflight_finished = Instant::now();

    if !dry_run {
        for (path, expected) in &versions {
            verify_file_version(ws, path, expected.as_deref())?;
        }
        let _backups = commit_staged(ws, &staged)?;
    }
    let commit_finished = Instant::now();
    let modified = affected
        .iter()
        .filter_map(|item| item.get("path").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let edit_plan = if dry_run {
        let replay_files = files
            .iter()
            .zip(file_versions.iter())
            .map(|(file, version)| {
                json!({
                    "path": version["path"],
                    "edits": file["edits"],
                    "expected_sha256": version["before_sha256"]
                })
            })
            .collect::<Vec<_>>();
        let mut replay_arguments = serde_json::Map::new();
        replay_arguments.insert("files".into(), Value::Array(replay_files));
        replay_arguments.insert("dry_run".into(), Value::Bool(false));
        if let Some(reason) = replay_reason(args) {
            replay_arguments.insert("reason".into(), reason);
        }
        replayable_edit_plan(
            "edit",
            Value::Object(replay_arguments),
            Value::Array(file_versions.clone()),
            json!([]),
        )
    } else {
        Value::Null
    };
    let completed = Instant::now();
    let phase_durations_ms = json!({
        "preflight_ms": preflight_finished.duration_since(started).as_millis(),
        "commit_ms": commit_finished.duration_since(preflight_finished).as_millis(),
        "plan_ms": completed.duration_since(commit_finished).as_millis(),
        "total_ms": completed.duration_since(started).as_millis()
    });
    Ok(tool_ok(json!({
        "dry_run": dry_run,
        "preflight": true,
        "applied": !dry_run,
        "atomic": true,
        "change_id": if dry_run { Value::Null } else { json!(Uuid::new_v4().simple().to_string()) },
        "diff": diffs,
        "file_versions": file_versions,
        "affected_files": affected,
        "edit_plan": edit_plan,
        "phase_durations_ms": phase_durations_ms,
        "files_created": Vec::<String>::new(),
        "files_modified": modified,
        "files_deleted": Vec::<String>::new(),
        "warnings": []
    })))
}

pub fn file_ops(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let operations = args
        .get("operations")
        .and_then(Value::as_array)
        .ok_or_else(|| WorkspaceError::invalid_argument("operations is required"))?;
    if operations.is_empty() {
        return Err(WorkspaceError::invalid_argument(
            "operations must not be empty",
        ));
    }
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let confirm = args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ws = &ctx.workspace;
    let mut staged: HashMap<String, Option<Vec<u8>>> = HashMap::new();
    let mut versions: HashMap<String, Option<String>> = HashMap::new();
    let mut directories = Vec::new();
    let mut affected = Vec::new();
    let mut diffs = String::new();
    let mut touched = HashSet::new();

    for (index, operation) in operations.iter().enumerate() {
        let kind = operation
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| WorkspaceError::invalid_argument("operations[].type is required"))?;
        let path = operation
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| WorkspaceError::invalid_argument("operations[].path is required"))?;
        ws.reject_unsafe_text(path)?;
        ws.reject_write_symlink(path)?;
        ws.reject_protected_write_path(path)?;
        match kind {
            "create" => {
                let resolved = ws.resolve_for_write(path)?;
                let overwrite = operation
                    .get("overwrite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if resolved.existed && overwrite && !confirm {
                    return Err(dangerous_operation(format!(
                        "Overwriting an existing file requires confirm=true: {path}"
                    )));
                }
                if resolved.existed && !overwrite {
                    return Err(WorkspaceError::ToolDetails {
                        code: "FILE_ALREADY_EXISTS",
                        message: format!("Create target already exists: {path}"),
                        category: "conflict",
                        retryable: false,
                        details: json!({"path": path, "operation_index": index}),
                    });
                }
                if !touched.insert(resolved.display.clone()) {
                    return Err(WorkspaceError::invalid_argument(format!(
                        "duplicate file_ops target: {path}"
                    )));
                }
                let before = if resolved.existed {
                    Some(fs::read(&resolved.path).map_err(|e| patch_failed(e.to_string()))?)
                } else {
                    None
                };
                check_operation_hash(operation, &resolved.display, before.as_deref())?;
                versions.insert(resolved.display.clone(), before.as_deref().map(sha256_hex));
                let content = operation
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .as_bytes()
                    .to_vec();
                if let (Some(before_text), Ok(after_text)) = (
                    before.as_ref().and_then(|b| std::str::from_utf8(b).ok()),
                    std::str::from_utf8(&content),
                ) {
                    diffs.push_str(&unified_diff(
                        &resolved.display,
                        before_text,
                        after_text,
                        false,
                        false,
                    ));
                } else if let Ok(after_text) = std::str::from_utf8(&content) {
                    diffs.push_str(&unified_diff(
                        &resolved.display,
                        "",
                        after_text,
                        true,
                        false,
                    ));
                }
                staged.insert(resolved.display.clone(), Some(content));
                affected.push(json!({"path": resolved.display, "operation": if resolved.existed {"update"} else {"add"}}));
            }
            "delete" => {
                if is_critical_file(path) && !confirm {
                    return Err(dangerous_operation(format!(
                        "Deleting a critical project file requires confirm=true: {path}"
                    )));
                }
                let resolved = ws.resolve_existing(path)?;
                if !resolved.path.is_file() {
                    return Err(WorkspaceError::invalid_argument(format!(
                        "delete target must be a file: {path}"
                    )));
                }
                if !touched.insert(resolved.display.clone()) {
                    return Err(WorkspaceError::invalid_argument(format!(
                        "duplicate file_ops target: {path}"
                    )));
                }
                let before = fs::read(&resolved.path).map_err(|e| patch_failed(e.to_string()))?;
                check_operation_hash(operation, &resolved.display, Some(&before))?;
                versions.insert(resolved.display.clone(), Some(sha256_hex(&before)));
                if let Ok(before_text) = std::str::from_utf8(&before) {
                    diffs.push_str(&unified_diff(
                        &resolved.display,
                        before_text,
                        "",
                        false,
                        true,
                    ));
                }
                staged.insert(resolved.display.clone(), None);
                affected.push(json!({"path": resolved.display, "operation": "delete"}));
            }
            "copy" | "move" => {
                let source = ws.resolve_existing(path)?;
                if !source.path.is_file() {
                    return Err(WorkspaceError::invalid_argument(format!(
                        "{kind} source must be a file: {path}"
                    )));
                }
                let destination = operation
                    .get("destination")
                    .and_then(Value::as_str)
                    .ok_or_else(|| {
                        WorkspaceError::invalid_argument("copy/move destination is required")
                    })?;
                ws.reject_unsafe_text(destination)?;
                ws.reject_write_symlink(destination)?;
                ws.reject_protected_write_path(destination)?;
                let target = ws.resolve_for_write(destination)?;
                if source.display == target.display {
                    return Err(WorkspaceError::invalid_argument(
                        "source and destination must differ",
                    ));
                }
                let overwrite = operation
                    .get("overwrite")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if target.existed && overwrite && !confirm {
                    return Err(dangerous_operation(format!(
                        "Overwriting a destination requires confirm=true: {destination}"
                    )));
                }
                if target.existed && !overwrite {
                    return Err(WorkspaceError::ToolDetails {
                        code: "FILE_ALREADY_EXISTS",
                        message: format!("Destination already exists: {destination}"),
                        category: "conflict",
                        retryable: false,
                        details: json!({"path": destination, "operation_index": index}),
                    });
                }
                if !touched.insert(target.display.clone())
                    || (kind == "move" && !touched.insert(source.display.clone()))
                {
                    return Err(WorkspaceError::invalid_argument(
                        "file_ops paths may only be touched once per transaction",
                    ));
                }
                let bytes = fs::read(&source.path).map_err(|e| patch_failed(e.to_string()))?;
                check_operation_hash(operation, &source.display, Some(&bytes))?;
                versions.insert(source.display.clone(), Some(sha256_hex(&bytes)));
                versions.insert(
                    target.display.clone(),
                    if target.existed {
                        Some(sha256_hex(
                            &fs::read(&target.path).map_err(|e| patch_failed(e.to_string()))?,
                        ))
                    } else {
                        None
                    },
                );
                staged.insert(target.display.clone(), Some(bytes));
                if kind == "move" {
                    staged.insert(source.display.clone(), None);
                    affected.push(json!({"path": source.display, "destination": target.display, "operation": "move"}));
                } else {
                    affected.push(json!({"path": source.display, "destination": target.display, "operation": "copy"}));
                }
            }
            "mkdir" => {
                let resolved = ws.resolve_for_write(path)?;
                if resolved.existed && !resolved.path.is_dir() {
                    return Err(WorkspaceError::invalid_argument(format!(
                        "mkdir target is a file: {path}"
                    )));
                }
                directories.push((
                    resolved.display.clone(),
                    resolved.path.clone(),
                    resolved.existed,
                ));
                affected.push(json!({"path": resolved.display, "operation": "mkdir"}));
            }
            _ => {
                return Err(WorkspaceError::invalid_argument(format!(
                    "unsupported file operation: {kind}"
                )))
            }
        }
    }

    if !dry_run {
        for (path, expected) in &versions {
            verify_file_version(ws, path, expected.as_deref())?;
        }
        let backups = commit_staged_bytes(ws, &staged)?;
        let mut created_dirs = Vec::new();
        for (_, path, existed) in &directories {
            if !*existed {
                if let Err(error) = fs::create_dir_all(path) {
                    restore_backups(&backups);
                    for created in created_dirs.iter().rev() {
                        let _ = fs::remove_dir(created);
                    }
                    return Err(patch_failed(format!("Failed to create directory: {error}")));
                }
                created_dirs.push(path.clone());
            }
        }
    }

    let created = affected
        .iter()
        .filter(|v| v["operation"] == "add")
        .filter_map(|v| v["path"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    let modified = affected
        .iter()
        .filter(|v| v["operation"] == "update")
        .filter_map(|v| v["path"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    let deleted = affected
        .iter()
        .filter(|v| v["operation"] == "delete")
        .filter_map(|v| v["path"].as_str().map(str::to_string))
        .collect::<Vec<_>>();
    Ok(tool_ok(json!({
        "dry_run": dry_run,
        "preflight": true,
        "applied": !dry_run,
        "atomic": true,
        "change_id": if dry_run { Value::Null } else { json!(Uuid::new_v4().simple().to_string()) },
        "diff": diffs,
        "affected_files": affected,
        "files_created": created,
        "files_modified": modified,
        "files_deleted": deleted,
        "warnings": []
    })))
}

fn check_operation_hash(
    operation: &Value,
    path: &str,
    bytes: Option<&[u8]>,
) -> Result<(), WorkspaceError> {
    if let Some(expected) = operation.get("expected_sha256").and_then(Value::as_str) {
        let actual = bytes.map(sha256_hex).unwrap_or_else(|| "missing".into());
        if !expected.eq_ignore_ascii_case(&actual) {
            return Err(version_mismatch(path, expected, &actual));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct FilePatch {
    path: String,
    hunks: Vec<Hunk>,
    is_new_file: bool,
    is_deleted: bool,
}

#[derive(Debug)]
struct Hunk {
    old_start: Option<usize>,
    lines: Vec<HunkLine>,
}

#[derive(Debug)]
enum HunkLine {
    Context(String),
    Add(String),
    Remove(String),
}

fn parse_unified_diff(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    if patch
        .lines()
        .any(|line| line.trim_end_matches('\r') == "*** Begin Patch")
    {
        return parse_codex_patch(patch);
    }

    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for line in patch.lines() {
        if line.starts_with("--- ") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            if let Some(f) = current.take() {
                files.push(f);
            }
            let path = parse_diff_path(line.strip_prefix("--- ").unwrap_or(""));
            current = Some(FilePatch {
                path,
                hunks: Vec::new(),
                is_new_file: line.contains("/dev/null"),
                is_deleted: false,
            });
        } else if line.starts_with("+++ ") {
            if let Some(ref mut f) = current {
                let new_path = parse_diff_path(line.strip_prefix("+++ ").unwrap_or(""));
                if !new_path.is_empty() && new_path != "/dev/null" {
                    f.path = new_path;
                }
                if line.contains("/dev/null") {
                    f.is_deleted = true;
                }
            }
        } else if line.starts_with("@@") {
            if let Some(h) = current_hunk.take() {
                if let Some(ref mut f) = current {
                    f.hunks.push(h);
                }
            }
            current_hunk = Some(Hunk {
                old_start: parse_hunk_old_start(line),
                lines: Vec::new(),
            });
        } else if let Some(ref mut hunk) = current_hunk {
            if let Some(rest) = line.strip_prefix('+') {
                hunk.lines.push(HunkLine::Add(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix('-') {
                hunk.lines.push(HunkLine::Remove(rest.to_string()));
            } else if let Some(rest) = line.strip_prefix(' ') {
                hunk.lines.push(HunkLine::Context(rest.to_string()));
            } else if line.is_empty() {
                hunk.lines.push(HunkLine::Context(String::new()));
            }
        }
    }
    if let Some(h) = current_hunk.take() {
        if let Some(ref mut f) = current {
            f.hunks.push(h);
        }
    }
    if let Some(f) = current.take() {
        files.push(f);
    }
    Ok(files)
}

fn parse_codex_patch(patch: &str) -> Result<Vec<FilePatch>, WorkspaceError> {
    let mut files = Vec::new();
    let mut current: Option<FilePatch> = None;
    let mut current_hunk: Option<Hunk> = None;

    for raw_line in patch.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line == "*** Begin Patch" {
            continue;
        }
        if line == "*** End Patch" {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            continue;
        }

        let header = line
            .strip_prefix("*** Add File: ")
            .map(|path| (path, true, false))
            .or_else(|| {
                line.strip_prefix("*** Update File: ")
                    .map(|path| (path, false, false))
            })
            .or_else(|| {
                line.strip_prefix("*** Delete File: ")
                    .map(|path| (path, false, true))
            });
        if let Some((path, is_new_file, is_deleted)) = header {
            finish_codex_file(&mut files, &mut current, &mut current_hunk);
            current = Some(FilePatch {
                path: parse_diff_path(path),
                hunks: Vec::new(),
                is_new_file,
                is_deleted,
            });
            if is_new_file {
                current_hunk = Some(Hunk {
                    old_start: Some(1),
                    lines: Vec::new(),
                });
            }
            continue;
        }

        if line.starts_with("@@") {
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current {
                    file.hunks.push(hunk);
                }
            }
            current_hunk = Some(Hunk {
                old_start: parse_hunk_old_start(line),
                lines: Vec::new(),
            });
            continue;
        }

        let Some(file) = current.as_ref() else {
            continue;
        };
        if file.is_deleted {
            continue;
        }
        let hunk = current_hunk.get_or_insert_with(|| Hunk {
            old_start: None,
            lines: Vec::new(),
        });
        if let Some(rest) = line.strip_prefix('+') {
            hunk.lines.push(HunkLine::Add(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix('-') {
            hunk.lines.push(HunkLine::Remove(rest.to_string()));
        } else if let Some(rest) = line.strip_prefix(' ') {
            hunk.lines.push(HunkLine::Context(rest.to_string()));
        } else if line.is_empty() {
            hunk.lines.push(HunkLine::Context(String::new()));
        }
    }

    finish_codex_file(&mut files, &mut current, &mut current_hunk);
    Ok(files)
}

fn finish_codex_file(
    files: &mut Vec<FilePatch>,
    current: &mut Option<FilePatch>,
    current_hunk: &mut Option<Hunk>,
) {
    if let Some(hunk) = current_hunk.take() {
        if let Some(file) = current.as_mut() {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current.take() {
        files.push(file);
    }
}

#[derive(Debug, Clone)]
struct ResolvedEdit {
    input_index: usize,
    start_byte: usize,
    end_byte: usize,
    replacement: String,
}

fn validate_precise_edit_contract(edits: &[Value]) -> Result<(), WorkspaceError> {
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

fn apply_precise_edits(original: &str, edits: &[Value]) -> Result<String, WorkspaceError> {
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

fn whitespace_text_candidates(
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

fn required_edit_text<'a>(
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

fn expected_occurrences(edit: &Value) -> usize {
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

fn adapt_newlines_to_original(value: &str, original: &str) -> String {
    let normalized = normalize_newlines(value);
    if preferred_line_ending(original) == "\r\n" {
        normalized.replace('\n', "\r\n")
    } else {
        normalized
    }
}

fn line_range_bytes(
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

fn byte_to_line(content: &str, byte: usize) -> usize {
    content[..byte]
        .bytes()
        .filter(|value| *value == b'\n')
        .count()
        + 1
}

fn unified_diff(
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

fn replayable_edit_plan(
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

fn replay_reason(args: &Value) -> Option<Value> {
    args.get("reason")
        .and_then(Value::as_str)
        .filter(|reason| !reason.is_empty())
        .map(|reason| json!(reason))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

fn version_mismatch(path: &str, expected: &str, actual: &str) -> WorkspaceError {
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

fn enrich_edit_error(error: WorkspaceError, path: &str, actual_sha256: &str) -> WorkspaceError {
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

fn enrich_edit_many_error(
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

fn expected_hash_for<'a>(args: &'a Value, path: &str) -> Option<&'a str> {
    let hashes = args.get("expected_sha256")?.as_object()?;
    hashes
        .get(path)
        .or_else(|| hashes.get(&path.replace('\\', "/")))
        .and_then(Value::as_str)
}

fn verify_file_version(
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

fn affected_paths(affected: &[Value], operation: &str) -> Vec<String> {
    affected
        .iter()
        .filter(|file| file["operation"] == operation)
        .filter_map(|file| file["path"].as_str().map(str::to_string))
        .collect()
}

fn parse_diff_path(raw: &str) -> String {
    let trimmed = raw.trim();
    let path = trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed);
    if path == "/dev/null" {
        return String::new();
    }
    path.replace('\\', "/")
}

fn parse_hunk_old_start(header: &str) -> Option<usize> {
    let old_range = header
        .strip_prefix("@@")?
        .trim_start()
        .strip_prefix('-')?
        .split_whitespace()
        .next()?;
    old_range
        .split(',')
        .next()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|line| line.max(1))
}

fn apply_hunks(original: &str, hunks: &[Hunk]) -> Result<String, WorkspaceError> {
    let line_ending = if original.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let had_trailing_newline = original.ends_with('\n');
    let mut lines: Vec<String> = if original.is_empty() {
        Vec::new()
    } else {
        original
            .split_terminator('\n')
            .map(|line| line.trim_end_matches('\r').to_string())
            .collect()
    };
    let mut offset: i64 = 0;
    let mut issues = Vec::<WorkspaceError>::new();

    for (hunk_index, hunk) in hunks.iter().enumerate() {
        let hunk_old: Vec<String> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(s) | HunkLine::Remove(s) => Some(s.clone()),
                HunkLine::Add(_) => None,
            })
            .collect();

        let preferred = hunk.old_start.map(|line| {
            ((line.saturating_sub(1)) as i64 + offset)
                .max(0)
                .min(lines.len() as i64) as usize
        });
        let pos = match find_hunk_position(&lines, &hunk_old, preferred, hunk_index) {
            Ok(position) => position,
            Err(error) => {
                issues.push(error);
                continue;
            }
        };

        let mut idx = pos;
        let mut added = 0i64;
        let mut removed = 0i64;
        for hl in &hunk.lines {
            match hl {
                HunkLine::Context(_) => idx += 1,
                HunkLine::Remove(_) => {
                    if idx < lines.len() {
                        lines.remove(idx);
                        removed += 1;
                    }
                }
                HunkLine::Add(s) => {
                    lines.insert(idx, s.clone());
                    idx += 1;
                    added += 1;
                }
            }
        }
        offset += added - removed;
    }
    if issues.len() == 1 {
        return Err(issues.pop().expect("single patch issue"));
    }
    if !issues.is_empty() {
        let issue_values = issues
            .iter()
            .map(WorkspaceError::to_error_value)
            .collect::<Vec<_>>();
        return Err(WorkspaceError::ToolDetails {
            code: "PATCH_PREFLIGHT_FAILED",
            message: format!("{} patch hunks failed preflight.", issue_values.len()),
            category: "validation",
            retryable: false,
            details: json!({
                "issue_count": issue_values.len(),
                "issues": issue_values,
                "recommended_tool": "edit",
                "suggestion": "Resolve all listed hunk issues before retrying. Prefer edit for precise replacements.",
                "recovery_actions": [{
                    "action": "switch_to_precise_edits",
                    "tool": "edit",
                    "required_arguments": ["files"],
                    "reason": "multiple_patch_hunks_failed_preflight"
                }]
            }),
        });
    }
    let mut output = lines.join(line_ending);
    if !output.is_empty() && (had_trailing_newline || original.is_empty()) {
        output.push_str(line_ending);
    }
    Ok(output)
}

fn find_hunk_position(
    lines: &[String],
    pattern: &[String],
    preferred: Option<usize>,
    hunk_index: usize,
) -> Result<usize, WorkspaceError> {
    if pattern.is_empty() {
        return Ok(preferred.unwrap_or(lines.len()).min(lines.len()));
    }
    if let Some(position) = preferred {
        if hunk_matches_at(lines, pattern, position) {
            return Ok(position);
        }
    }

    let mut candidates = Vec::new();
    if pattern.len() <= lines.len() {
        for position in 0..=lines.len() - pattern.len() {
            if hunk_matches_at(lines, pattern, position) {
                candidates.push(position);
            }
        }
    }
    match candidates.as_slice() {
        [position] => Ok(*position),
        [] => Err(WorkspaceError::ToolDetails {
            code: "PATCH_CONTEXT_NOT_FOUND",
            message: format!("Hunk {hunk_index} context did not match file content."),
            category: "validation",
            retryable: false,
            details: json!({
                "hunk_index": hunk_index,
                "preferred_line": preferred.map(|line| line + 1),
                "pattern_preview": pattern.iter().take(8).collect::<Vec<_>>(),
                "nearby_contexts": preferred
                    .map(|position| nearby_contexts(lines, &[position], 3))
                    .unwrap_or_default(),
                "recommended_tool": "edit",
                "suggestion": "Read the exact target range and use edit for a single precise replacement, or include more unique patch context.",
                "recovery_actions": [{
                    "action": "read_target_range",
                    "tool": "read_file",
                    "required_arguments": ["path"],
                    "arguments": {
                        "start_line": preferred.map(|line| line.saturating_sub(3).max(1)),
                        "end_line": preferred.map(|line| line.saturating_add(4))
                    },
                    "reason": "patch_context_not_found"
                }, {
                    "action": "switch_to_precise_edit",
                    "tool": "edit",
                    "required_arguments": ["files"],
                    "reason": "patch_context_not_found"
                }]
            }),
        }),
        _ => Err(WorkspaceError::ToolDetails {
            code: "PATCH_CONTEXT_AMBIGUOUS",
            message: format!(
                "Hunk {hunk_index} context matched multiple locations; add more context or line numbers."
            ),
            category: "validation",
            retryable: false,
            details: json!({
                "hunk_index": hunk_index,
                "candidate_lines": candidates
                    .iter()
                    .map(|position| position + 1)
                    .collect::<Vec<_>>(),
                "nearby_contexts": nearby_contexts(lines, &candidates, 3),
                "recommended_tool": "edit",
                "suggestion": "Use edit with exact old_text and expected_sha256, or add unique surrounding lines to this hunk.",
                "recovery_actions": [{
                    "action": "select_candidate_range",
                    "tool": "edit",
                    "required_arguments": ["files"],
                    "candidate_lines": candidates
                        .iter()
                        .map(|position| position + 1)
                        .collect::<Vec<_>>(),
                    "reason": "patch_context_ambiguous"
                }]
            }),
        }),
    }
}

fn nearby_contexts(lines: &[String], positions: &[usize], radius: usize) -> Vec<Value> {
    positions
        .iter()
        .take(8)
        .map(|position| {
            let start = position.saturating_sub(radius);
            let end = (position.saturating_add(radius + 1)).min(lines.len());
            json!({
                "line": position + 1,
                "start_line": start + 1,
                "end_line": end,
                "preview": lines[start..end]
            })
        })
        .collect()
}

fn hunk_matches_at(lines: &[String], pattern: &[String], position: usize) -> bool {
    position <= lines.len()
        && pattern.len() <= lines.len().saturating_sub(position)
        && lines[position..position + pattern.len()]
            .iter()
            .zip(pattern.iter())
            .all(|(actual, expected)| actual == expected)
}

fn commit_staged(
    ws: &Workspace,
    staged: &HashMap<String, Option<String>>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let staged_bytes = staged
        .iter()
        .map(|(path, content)| {
            (
                path.clone(),
                content.as_ref().map(|value| value.as_bytes().to_vec()),
            )
        })
        .collect::<HashMap<_, _>>();
    commit_staged_bytes(ws, &staged_bytes)
}

pub(crate) fn commit_staged_bytes(
    ws: &Workspace,
    staged: &HashMap<String, Option<Vec<u8>>>,
) -> Result<HashMap<PathBuf, Option<Vec<u8>>>, WorkspaceError> {
    let mut backups: HashMap<PathBuf, Option<Vec<u8>>> = HashMap::new();
    let mut temporary_files = HashMap::new();
    for (rel, content) in staged {
        ws.reject_protected_write_path(rel)?;
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path.clone();
        backups.insert(
            path.clone(),
            if path.exists() && path.is_file() {
                Some(fs::read(&path).unwrap_or_default())
            } else {
                None
            },
        );
        if let Some(bytes) = content {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|err| patch_failed(err.to_string()))?;
            }
            let temp = path.with_file_name(format!(
                ".{}.harness-stage-{}",
                path.file_name().and_then(|v| v.to_str()).unwrap_or("file"),
                Uuid::new_v4().simple()
            ));
            if let Err(err) = fs::write(&temp, bytes) {
                cleanup_temporary_files(temporary_files.values());
                restore_backups(&backups);
                return Err(patch_failed(format!("Failed to stage file: {err}")));
            }
            temporary_files.insert(path.clone(), temp);
        }
    }

    for (rel, content) in staged {
        let resolved = if content.is_none() {
            ws.resolve_existing(rel)?
        } else {
            ws.resolve_for_write(rel)?
        };
        let path = resolved.path;
        let result = if content.is_some() {
            let temp = temporary_files
                .get(&path)
                .cloned()
                .ok_or_else(|| patch_failed("Staged file is missing"));
            match temp {
                Ok(temp) => replace_file(&temp, &path),
                Err(error) => Err(std::io::Error::other(error.to_string())),
            }
        } else if path.exists() && path.is_file() {
            fs::remove_file(&path)
        } else {
            Ok(())
        };
        if let Err(err) = result {
            cleanup_temporary_files(temporary_files.values());
            restore_backups(&backups);
            return Err(patch_failed(format!("Failed to write file: {err}")));
        }
    }
    cleanup_temporary_files(temporary_files.values());
    Ok(backups)
}

fn restore_backups(backups: &HashMap<PathBuf, Option<Vec<u8>>>) {
    for (path, data) in backups {
        match data {
            None => {
                let _ = fs::remove_file(path);
            }
            Some(bytes) => {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                let _ = fs::write(path, bytes);
            }
        }
    }
}

fn replace_file(temp: &PathBuf, path: &PathBuf) -> Result<(), std::io::Error> {
    #[cfg(windows)]
    {
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    fs::rename(temp, path)
}

fn cleanup_temporary_files<'a>(paths: impl Iterator<Item = &'a PathBuf>) {
    for path in paths {
        let _ = fs::remove_file(path);
    }
}

fn is_critical_file(path: &str) -> bool {
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

fn is_protected_repository_asset(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let first = normalized.split('/').next().unwrap_or("");
    matches!(first, ".git" | ".github")
}

fn dangerous_operation(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
        message: message.into(),
        category: "permission",
        retryable: false,
    }
}

fn protected_repository_asset(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PROTECTED_REPOSITORY_ASSET",
        message: message.into(),
        category: "security",
        retryable: false,
    }
}

fn patch_failed(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "PATCH_FAILED",
        message: message.into(),
        category: "validation",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::context::ToolContext;
    use serde_json::json;
    use tempfile::tempdir;

    fn context_with_file() -> (tempfile::TempDir, tempfile::TempDir, ToolContext) {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(workspace.path().join("main.rs"), "old\n").expect("file");
        let context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        (workspace, harness, context)
    }

    fn patch() -> Value {
        json!({
            "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n"
        })
    }

    #[test]
    fn patch_check_does_not_modify_workspace() {
        let (_workspace, _harness, context) = context_with_file();
        let result = patch_check(&context, &patch()).expect("patch check");
        assert_eq!(result["preflight"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn preserves_crlf_when_inserting_multiple_lines() {
        let input = "one\r\ntwo\r\n";
        let hunk = Hunk {
            old_start: Some(1),
            lines: vec![
                HunkLine::Context("one".into()),
                HunkLine::Add("insert-a".into()),
                HunkLine::Add("insert-b".into()),
                HunkLine::Context("two".into()),
            ],
        };
        assert_eq!(
            apply_hunks(input, &[hunk]).expect("patch"),
            "one\r\ninsert-a\r\ninsert-b\r\ntwo\r\n"
        );
    }

    #[test]
    fn delete_then_add_same_path_replaces_instead_of_concatenating_old_content() {
        let (_workspace, _harness, context) = context_with_file();
        let result = apply_patch(
            &context,
            &json!({
                "patch": "*** Begin Patch\n*** Delete File: main.rs\n*** Add File: main.rs\n+fresh\n*** End Patch\n"
            }),
        )
        .expect("replace file");
        assert_eq!(result["files_modified"], json!(["main.rs"]));
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "fresh\n"
        );
    }

    #[test]
    fn validation_failure_in_later_file_keeps_all_files_unchanged() {
        let (_workspace, _harness, context) = context_with_file();
        let error = apply_patch(
            &context,
            &json!({
                "patch": "--- a/main.rs\n+++ b/main.rs\n@@\n-old\n+new\n--- a/missing.rs\n+++ b/missing.rs\n@@\n-old\n+new\n"
            }),
        )
        .expect_err("later file fails preflight");
        assert_eq!(error.to_error_value()["code"], "NOT_FOUND");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn edit_file_requires_exact_match_and_returns_diff() {
        let (_workspace, _harness, context) = context_with_file();
        let before = sha256_hex(b"old\n");
        let result = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "expected_sha256": before,
                "edits": [{
                    "type": "replace",
                    "old_text": "old",
                    "new_text": "new",
                    "expected_occurrences": 1
                }]
            }),
        )
        .expect("edit file");
        assert!(result["diff"].as_str().unwrap().contains("+new"));
        assert_eq!(result["before_sha256"].as_str().unwrap().len(), 64);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn dry_run_edit_plan_replays_once_and_rejects_stale_reuse() {
        let (_workspace, _harness, context) = context_with_file();
        let planned = edit(
            &context,
            &json!({
                "files": [{
                    "path": "main.rs",
                    "edits": [{
                        "type": "replace",
                        "old_text": "old",
                        "new_text": "new"
                    }]
                }],
                "dry_run": true,
                "reason": "guarded replay test"
            }),
        )
        .expect("dry-run plan");
        let plan = planned["edit_plan"].clone();
        assert_eq!(plan["tool"], "edit");
        assert_eq!(plan["arguments"]["dry_run"], false);
        assert_eq!(plan["arguments"]["reason"], "guarded replay test");
        assert_eq!(
            plan["arguments"]["files"][0]["expected_sha256"],
            planned["before_sha256"]
        );
        assert_eq!(plan["stateful_dependencies"], json!([]));
        assert_eq!(plan["plan_sha256"].as_str().unwrap().len(), 64);

        let replayed = edit(&context, &plan["arguments"]).expect("replay plan");
        assert_eq!(replayed["applied"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "new\n"
        );

        let stale = edit(&context, &plan["arguments"]).expect_err("stale replay");
        assert_eq!(stale.to_error_value()["code"], "FILE_VERSION_MISMATCH");
    }

    #[test]
    fn dry_run_edit_many_plan_replays_atomically() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(context.workspace.root().join("second.rs"), "second\n")
            .expect("second fixture");
        let planned = edit(
            &context,
            &json!({
                "dry_run": true,
                "files": [
                    {
                        "path": "main.rs",
                        "edits": [{ "type": "replace", "old_text": "old", "new_text": "NEW" }]
                    },
                    {
                        "path": "second.rs",
                        "edits": [{ "type": "replace", "old_text": "second", "new_text": "SECOND" }]
                    }
                ]
            }),
        )
        .expect("edit-many plan");
        let plan = planned["edit_plan"].clone();
        assert_eq!(plan["tool"], "edit");
        assert_eq!(plan["arguments"]["files"].as_array().unwrap().len(), 2);
        assert_eq!(plan["arguments"]["dry_run"], false);
        assert_eq!(plan["plan_sha256"].as_str().unwrap().len(), 64);

        let replayed = edit(&context, &plan["arguments"]).expect("replay edit");
        assert_eq!(replayed["applied"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "NEW\n"
        );
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("second.rs")).unwrap(),
            "SECOND\n"
        );
    }

    #[test]
    fn edit_reports_ambiguous_candidates_with_context_without_writing() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "fn first() {\n    return value;\n}\n\nfn second() {\n    return value;\n}\n",
        )
        .expect("fixture");

        let error = edit(
            &context,
            &json!({
                "files": [{
                    "path": "main.rs",
                    "edits": [{
                        "type": "replace",
                        "old_text": "return value;",
                        "new_text": "return result;"
                    }]
                }]
            }),
        )
        .expect_err("ambiguous target");
        let value = error.to_error_value();
        assert_eq!(value["code"], "EDIT_MATCH_COUNT_MISMATCH");
        assert_eq!(value["details"]["actual_occurrences"], 2);
        assert_eq!(value["details"]["candidate_lines"], json!([2, 6]));
        assert_eq!(
            value["details"]["candidate_contexts"]
                .as_array()
                .expect("candidate contexts")
                .len(),
            2
        );
        assert_eq!(value["details"]["candidate_contexts_truncated"], false);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "fn first() {\n    return value;\n}\n\nfn second() {\n    return value;\n}\n"
        );
    }

    #[test]
    fn edit_rejects_proposal_mode_for_multiple_files() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(context.workspace.root().join("second.rs"), "second\n")
            .expect("second fixture");
        let error = edit(
            &context,
            &json!({
                "files": [
                    {
                        "path": "main.rs",
                        "apply_proposal": { "proposal_id": "proposal" }
                    },
                    {
                        "path": "second.rs",
                        "edits": [{ "type": "replace", "old_text": "second", "new_text": "SECOND" }]
                    }
                ]
            }),
        )
        .expect_err("proposal requires one file");
        assert_eq!(error.to_error_value()["code"], "EDIT_CONTRACT_INVALID");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("second.rs")).unwrap(),
            "second\n"
        );
    }

    #[test]
    fn edit_file_exact_mode_tolerates_newline_style_and_preserves_crlf() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "fn main() {\r\n    let first = 1;\r\n    let second = 2;\r\n}\r\n",
        )
        .expect("fixture");

        let result = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "replace",
                    "old_text": "    let first = 1;\n    let second = 2;",
                    "new_text": "    let first = 10;\n    let second = 20;",
                    "before_context": "fn main() {\n",
                    "after_context": "\n}\n"
                }]
            }),
        )
        .expect("newline-compatible exact edit");

        assert_eq!(result["applied"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "fn main() {\r\n    let first = 10;\r\n    let second = 20;\r\n}\r\n"
        );
    }

    #[test]
    fn edit_file_normalizes_replacement_and_insert_text_to_file_style() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "alpha\r\nomega\r\n",
        )
        .expect("fixture");

        edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "insert_after",
                    "anchor": "alpha\n",
                    "text": "inserted\n"
                }]
            }),
        )
        .expect("newline-compatible insert");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "alpha\r\ninserted\r\nomega\r\n"
        );

        std::fs::write(context.workspace.root().join("main.rs"), "one\ntwo\n").expect("lf fixture");
        edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "replace",
                    "old_text": "one\r\ntwo",
                    "new_text": "first\r\nsecond"
                }]
            }),
        )
        .expect("lf-preserving replacement");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "first\nsecond\n"
        );
    }

    #[test]
    fn edit_file_whitespace_mode_tolerates_indent_and_crlf_differences() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "fn main() {\r\n    let value = 1;\r\n}\r\n",
        )
        .expect("fixture");
        let result = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "replace",
                    "old_text": "fn main() {\n  let value = 1;\n}",
                    "new_text": "fn main() {\r\n    let value = 2;\r\n}",
                    "match_mode": "whitespace"
                }]
            }),
        )
        .expect("whitespace edit");
        assert_eq!(result["applied"], true);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "fn main() {\r\n    let value = 2;\r\n}\r\n"
        );
    }

    #[test]
    fn edit_file_exact_failure_returns_cost_guidance_and_applies_replacement() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "let  value = 1;\n",
        )
        .expect("fixture");
        let proposal = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "replace",
                    "old_text": "let value = 1;",
                    "new_text": "let value = 2;"
                }]
            }),
        )
        .expect("proposal");
        assert_eq!(proposal["status"], "proposal_required");
        assert_eq!(proposal["applied"], false);
        assert_eq!(proposal["proposed_content_included"], true);
        assert_eq!(proposal["proposed_content"], "let value = 2;\n");
        assert_eq!(proposal["preferred_format"], "replacement");
        assert_eq!(proposal["replacement_bytes"], 14);
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "let  value = 1;\n"
        );

        let proposal_id = proposal["proposal_id"].as_str().unwrap();
        let result = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "apply_proposal": {
                    "proposal_id": proposal_id,
                    "replacement": "let value = 3;"
                }
            }),
        )
        .expect("apply proposal");
        assert_eq!(result["status"], "proposal_applied");
        assert_eq!(result["proposal_apply_format"], "replacement");
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "let value = 3;\n"
        );
    }

    fn apply_edit_proposal_for_test(proposed_text: &str, patch: &str) -> WorkspaceError {
        let patched = apply_restricted_proposal_patch(proposed_text, patch).expect("valid patch");
        if patch.len().saturating_mul(100) >= patched.len().saturating_mul(PATCH_EFFICIENCY_PERCENT)
        {
            WorkspaceError::ToolDetails {
                code: "EDIT_PROPOSAL_PATCH_INEFFICIENT",
                message:
                    "Proposal patch costs as much as or more than sending the full replacement."
                        .into(),
                category: "validation",
                retryable: true,
                details: json!({
                    "reason": "replacement_is_cheaper",
                    "patch_bytes": patch.len(),
                    "replacement_bytes": patched.len(),
                    "recommended_format": "replacement"
                }),
            }
        } else {
            panic!("fixture patch should be inefficient")
        }
    }

    #[test]
    fn inefficient_proposal_patch_recommends_full_replacement() {
        let error = apply_edit_proposal_for_test(
            "let value = 2;",
            "--- a/proposal\n+++ b/proposal\n@@\n-let value = 2;\n+let value = 3;\n",
        );
        assert_eq!(
            error.to_error_value()["code"],
            "EDIT_PROPOSAL_PATCH_INEFFICIENT"
        );
        assert_eq!(
            error.to_error_value()["details"]["recommended_format"],
            "replacement"
        );
    }

    #[test]
    fn restricted_proposal_patch_rejects_multiple_hunks() {
        let error = apply_restricted_proposal_patch(
            "one\ntwo\n",
            "--- a/proposal\n+++ b/proposal\n@@\n-one\n+ONE\n@@\n-two\n+TWO\n",
        )
        .expect_err("multiple hunks must be rejected");
        assert_eq!(
            error.to_error_value()["code"],
            "EDIT_PROPOSAL_PATCH_INVALID"
        );
    }

    #[test]
    fn edit_file_rejects_stale_proposal() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "let  value = 1;\n",
        )
        .expect("fixture");
        let proposal = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "replace",
                    "old_text": "let value = 1;",
                    "new_text": "let value = 2;"
                }]
            }),
        )
        .expect("proposal");
        std::fs::write(
            context.workspace.root().join("main.rs"),
            "let  value = 9;\n",
        )
        .expect("change fixture");
        let error = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "apply_proposal": {
                    "proposal_id": proposal["proposal_id"]
                }
            }),
        )
        .expect_err("stale proposal");
        assert_eq!(error.to_error_value()["code"], "EDIT_PROPOSAL_STALE");
    }

    #[test]
    fn edit_file_rejects_stale_hash() {
        let (_workspace, _harness, context) = context_with_file();
        let error = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "expected_sha256": "0".repeat(64),
                "edits": [{
                    "type": "replace",
                    "old_text": "old",
                    "new_text": "new"
                }]
            }),
        )
        .expect_err("stale hash");
        assert_eq!(error.to_error_value()["code"], "FILE_VERSION_MISMATCH");
    }

    #[test]
    fn edit_file_aggregates_contract_issues_with_guarded_recovery() {
        let (_workspace, _harness, context) = context_with_file();
        let error = edit_file(
            &context,
            &json!({
                "path": "main.rs",
                "edits": [{
                    "type": "replace",
                    "old_text": "old",
                    "anchor": "unexpected"
                }]
            }),
        )
        .expect_err("invalid edit contract");
        let value = error.to_error_value();
        assert_eq!(value["code"], "EDIT_CONTRACT_INVALID");
        assert_eq!(value["details"]["issue_count"], 2);
        assert_eq!(value["details"]["path"], "main.rs");
        assert!(value["details"]["actual_sha256"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64));
        assert_eq!(value["details"]["recovery_actions"][1]["tool"], "edit");
    }

    #[test]
    fn edit_many_contract_failure_identifies_file_and_preserves_atomicity() {
        let (_workspace, _harness, context) = context_with_file();
        std::fs::write(context.workspace.root().join("second.rs"), "second\n")
            .expect("second fixture");
        let error = edit_many(
            &context,
            &json!({
                "files": [
                    {
                        "path": "main.rs",
                        "edits": [{
                            "type": "replace",
                            "old_text": "old",
                            "new_text": "new"
                        }]
                    },
                    {
                        "path": "second.rs",
                        "edits": [{
                            "type": "replace",
                            "old_text": "second",
                            "anchor": "unexpected"
                        }]
                    }
                ]
            }),
        )
        .expect_err("second file contract failure");
        let value = error.to_error_value();
        assert_eq!(value["code"], "EDIT_CONTRACT_INVALID");
        assert_eq!(value["details"]["file_index"], 1);
        assert_eq!(value["details"]["path"], "second.rs");
        assert!(value["details"]["recovery_actions"].is_array());
        assert_eq!(
            std::fs::read_to_string(context.workspace.root().join("main.rs")).unwrap(),
            "old\n"
        );
    }

    #[test]
    fn patch_without_line_numbers_rejects_ambiguous_context() {
        let hunk = Hunk {
            old_start: None,
            lines: vec![
                HunkLine::Context("same".into()),
                HunkLine::Add("inserted".into()),
            ],
        };
        let error = apply_hunks("same\nother\nsame\n", &[hunk]).expect_err("ambiguous");
        assert_eq!(error.to_error_value()["code"], "PATCH_CONTEXT_AMBIGUOUS");
    }

    #[test]
    fn patch_preflight_reports_multiple_hunk_issues_together() {
        let hunks = vec![
            Hunk {
                old_start: None,
                lines: vec![HunkLine::Context("missing-one".into())],
            },
            Hunk {
                old_start: None,
                lines: vec![HunkLine::Context("missing-two".into())],
            },
        ];
        let error = apply_hunks("actual\ncontent\n", &hunks).expect_err("preflight issues");
        let value = error.to_error_value();
        assert_eq!(value["code"], "PATCH_PREFLIGHT_FAILED");
        assert_eq!(value["details"]["issue_count"], 2);
        assert_eq!(value["details"]["issues"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn apply_patch_checks_expected_hash_and_returns_versions() {
        let (_workspace, _harness, context) = context_with_file();
        let before = sha256_hex(b"old\n");
        let result = apply_patch(
            &context,
            &json!({
                "patch": "--- a/main.rs\n+++ b/main.rs\n@@ -1,1 +1,1 @@\n-old\n+new\n",
                "expected_sha256": { "main.rs": before.clone() }
            }),
        )
        .expect("apply patch");
        assert_eq!(result["preflight"], true);
        assert_eq!(result["applied"], true);
        assert!(result["diff"].as_str().unwrap().contains("+new"));
        assert_eq!(result["file_versions"][0]["before_sha256"], before);
        assert_eq!(
            result["file_versions"][0]["after_sha256"]
                .as_str()
                .unwrap()
                .len(),
            64
        );
    }
}
