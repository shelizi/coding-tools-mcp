use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use regex::Regex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::platform::wsl::std_command_for_workspace;
use crate::tools::workspace::{tool_ok, Workspace, WorkspaceError};

#[derive(Debug, Clone)]
struct GitTarget {
    repo_path: String,
    root: PathBuf,
    git_dir: String,
    common_dir: String,
    branch: String,
    head: String,
    fingerprint: String,
}

impl GitTarget {
    fn metadata(&self) -> Value {
        json!({
            "repo_path": self.repo_path,
            "repo_root": self.root.to_string_lossy(),
            "git_dir": self.git_dir,
            "git_common_dir": self.common_dir,
            "branch": self.branch,
            "head": self.head,
            "repo_fingerprint": self.fingerprint,
        })
    }
}

pub fn git_status(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(path)?;
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(1000) as usize;
    let include_untracked = args
        .get("include_untracked")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let root_check = run_git(
        &resolved.path,
        &["rev-parse", "--show-toplevel"],
        Duration::from_secs(10),
    )?;
    if !root_check.success {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "clean": true,
            "entries": [],
            "warnings": [root_check.stderr.trim()]
        })));
    }

    let target = resolve_git_target(ws, &json!({"repo_path": path}))?;

    let mut status_args = vec!["status", "--porcelain=v1", "-b"];
    if !include_untracked {
        status_args.push("--untracked-files=no");
    }
    let completed = run_git(&resolved.path, &status_args, Duration::from_secs(10))?;
    if !completed.success && completed.exit_code != 0 {
        return Err(git_error(&completed.stderr));
    }

    let mut branch = String::new();
    let mut upstream = String::new();
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut entries = Vec::new();
    let lines: Vec<_> = completed.stdout.lines().collect();
    let total_lines = lines.len();

    for line in lines {
        if let Some(rest) = line.strip_prefix("## ") {
            (branch, upstream, ahead, behind) = parse_branch_line(rest);
            continue;
        }
        if line.len() < 4 {
            continue;
        }
        let index_status = line.chars().next().unwrap_or(' ').to_string();
        let worktree_status = line.chars().nth(1).unwrap_or(' ').to_string();
        let mut path_text = line[3..].to_string();
        let original = if let Some((orig, new)) = path_text.split_once(" -> ") {
            let orig = orig.to_string();
            path_text = new.to_string();
            Some(orig)
        } else {
            None
        };
        let mut entry = json!({
            "path": path_text,
            "index_status": index_status,
            "worktree_status": worktree_status
        });
        if let Some(orig) = original {
            entry["original_path"] = json!(orig);
        }
        entries.push(entry);
        if entries.len() >= max_entries {
            break;
        }
    }

    let head = git_rev_parse(&resolved.path, "HEAD").unwrap_or_default();
    Ok(tool_ok(json!({
        "is_repo": true,
        "branch": branch,
        "head": head,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "clean": entries.is_empty(),
        "repo": target.metadata(),
        "repo_fingerprint": target.fingerprint,
        "entries": entries,
        "truncated": entries.len() >= max_entries && total_lines > max_entries + 1,
        "warnings": []
    })))
}

pub fn git_diff(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
    let unstaged = args
        .get("unstaged")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let requested_context = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(3);
    let context = requested_context.min(20);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262_144) as usize;

    let mut path_filters: Vec<String> = Vec::new();
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        path_filters.push(p.to_string());
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        for p in paths {
            if let Some(s) = p.as_str() {
                path_filters.push(s.to_string());
            }
        }
    }
    for p in &path_filters {
        ws.reject_unsafe_text(p)?;
    }

    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "diff": "",
            "files": [],
            "truncated": false,
            "warnings": ["not a git repository"]
        })));
    }

    let mut chunks = Vec::new();
    if unstaged {
        chunks.push(run_git_diff(ws.root(), context, &path_filters, false)?);
    }
    if staged {
        chunks.push(run_git_diff(ws.root(), context, &path_filters, true)?);
    }
    let mut combined = chunks.join("\n");
    if !combined.is_empty() && !combined.ends_with('\n') {
        combined.push('\n');
    }
    let truncated = combined.len() > max_bytes;
    let diff_text = if truncated {
        String::from_utf8_lossy(&combined.as_bytes()[..max_bytes]).into_owned()
    } else {
        combined
    };
    let files = parse_diff_files(&diff_text);
    Ok(tool_ok(json!({
        "diff": diff_text,
        "files": files,
        "arguments_normalized": requested_context != context,
        "normalized_arguments": if requested_context != context {
            json!({ "context_lines": context })
        } else {
            Value::Null
        },
        "truncated": truncated,
        "warnings": if truncated { vec!["diff truncated"] } else { vec![] }
    })))
}

pub fn git_log(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(path)?;
    let ref_name = validate_git_ref(args.get("ref").and_then(Value::as_str).unwrap_or("HEAD"))?;
    let max_count = args
        .get("max_count")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 100) as usize;
    let skip = args
        .get("skip")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(10_000) as usize;

    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "commits": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let max_count_arg = format!("--max-count={}", max_count + 1);
    let skip_arg = format!("--skip={skip}");
    let pretty = "--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%s%x1e";
    let path_filter = if resolved.display.is_empty() {
        ".".to_string()
    } else {
        resolved.display.clone()
    };
    let mut cmd_args = vec![
        "log",
        max_count_arg.as_str(),
        skip_arg.as_str(),
        "--date=iso-strict",
        pretty,
        ref_name,
    ];
    if path_filter != "." {
        cmd_args.push("--");
        cmd_args.push(path_filter.as_str());
    }

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let mut commits = Vec::new();
    for record in completed.stdout.split('\u{1e}') {
        let fields: Vec<String> = record
            .trim()
            .split('\u{1f}')
            .map(str::trim)
            .map(str::to_string)
            .collect();
        if fields.len() < 6 || fields[0].is_empty() {
            continue;
        }
        commits.push(json!({
            "hash": fields[0],
            "short_hash": fields[1],
            "author_name": fields[2],
            "author_email": fields[3],
            "author_date": fields[4],
            "subject": fields[5],
        }));
    }
    let truncated = commits.len() > max_count;
    Ok(tool_ok(json!({
        "is_repo": true,
        "ref": ref_name,
        "path": path_filter,
        "commits": commits.into_iter().take(max_count).collect::<Vec<_>>(),
        "truncated": truncated,
        "warnings": if truncated { vec!["commit limit reached"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_show(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "content": "",
            "files": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let rev = validate_git_ref(args.get("rev").and_then(Value::as_str).unwrap_or("HEAD"))?;
    let requested_context = args
        .get("context_lines")
        .and_then(Value::as_u64)
        .unwrap_or(3);
    let context = requested_context.min(20);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(262_144) as usize;
    let include_diff = args
        .get("include_diff")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    let mut path_filters: Vec<String> = Vec::new();
    if let Some(p) = args.get("path").and_then(Value::as_str) {
        path_filters.push(p.to_string());
    }
    if let Some(paths) = args.get("paths").and_then(Value::as_array) {
        for p in paths {
            if let Some(s) = p.as_str() {
                path_filters.push(s.to_string());
            }
        }
    }
    for p in &path_filters {
        ws.reject_unsafe_text(p)?;
    }

    let unified = format!("--unified={context}");
    let mut cmd_args = vec!["show", "--no-ext-diff", "--format=fuller", unified.as_str()];
    if !include_diff {
        cmd_args.push("--no-patch");
    }
    cmd_args.push(rev);
    if !path_filters.is_empty() {
        cmd_args.push("--");
        for p in &path_filters {
            cmd_args.push(p.as_str());
        }
    }

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let truncated = completed.stdout.len() > max_bytes;
    let content = if truncated {
        String::from_utf8_lossy(&completed.stdout.as_bytes()[..max_bytes]).into_owned()
    } else {
        completed.stdout.clone()
    };
    let files = parse_diff_files(&content);
    Ok(tool_ok(json!({
        "is_repo": true,
        "rev": rev,
        "content": content,
        "files": files,
        "arguments_normalized": requested_context != context,
        "normalized_arguments": if requested_context != context {
            json!({ "context_lines": context })
        } else {
            Value::Null
        },
        "truncated": truncated,
        "output_bytes": content.len(),
        "warnings": if truncated { vec!["output truncated"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_blame(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("path is required"))?;
    let resolved = ws.resolve_existing(path)?;
    if resolved.path.is_dir() {
        return Err(WorkspaceError::Tool {
            code: "IS_DIRECTORY",
            message: "Path is a directory.".into(),
            category: "validation",
            retryable: false,
        });
    }
    if !is_git_repo(ws.root()) {
        return Ok(tool_ok(json!({
            "is_repo": false,
            "path": resolved.display,
            "lines": [],
            "truncated": false,
            "warnings": []
        })));
    }

    let ref_arg = args.get("rev").and_then(Value::as_str);
    let git_ref = ref_arg.map(validate_git_ref).transpose()?;
    let start_line = args
        .get("start_line")
        .and_then(Value::as_u64)
        .unwrap_or(1)
        .max(1) as usize;
    let end_line_arg = args
        .get("end_line")
        .and_then(Value::as_u64)
        .map(|v| v as usize);
    let max_lines = args
        .get("max_lines")
        .and_then(Value::as_u64)
        .unwrap_or(200)
        .clamp(1, 1000) as usize;

    let final_line = match end_line_arg {
        None => start_line + max_lines - 1,
        Some(end) if end < start_line => {
            return Err(WorkspaceError::invalid_argument(
                "end_line must be >= start_line.",
            ));
        }
        Some(end) => end,
    };
    let requested_lines = final_line - start_line + 1;
    let mut truncated = requested_lines > max_lines;
    let final_line = final_line.min(start_line + max_lines - 1);

    let line_range = format!("{start_line},{final_line}");
    let mut cmd_args = vec!["blame", "--line-porcelain", "-L", line_range.as_str()];
    if let Some(r) = git_ref {
        cmd_args.push(r);
    }
    cmd_args.push("--");
    cmd_args.push(resolved.display.as_str());

    let completed = run_git(ws.root(), &cmd_args, Duration::from_secs(10))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }

    let mut lines = parse_git_blame_porcelain(&completed.stdout);
    if lines.len() > max_lines {
        lines.truncate(max_lines);
        truncated = true;
    }

    Ok(tool_ok(json!({
        "is_repo": true,
        "path": resolved.display,
        "rev": ref_arg,
        "start_line": start_line,
        "end_line": final_line,
        "lines": lines,
        "truncated": truncated,
        "warnings": if truncated { vec!["line limit reached"] } else { Vec::<&str>::new() }
    })))
}

pub fn git_branch(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let target = resolve_git_target(ws, args)?;
    verify_expected_head(&target, args)?;
    let action = args
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("action is required"))?;
    let name = args
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("name is required"))?;
    validate_branch_name(&target.root, name)?;
    let dry_run = args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let force = args.get("force").and_then(Value::as_bool).unwrap_or(false);
    let mut command = vec!["git".to_string()];
    let mut git_args = Vec::<String>::new();
    match action {
        "create" => {
            let switch = args.get("switch").and_then(Value::as_bool).unwrap_or(true);
            if switch {
                git_args.extend(["switch".into(), "-c".into(), name.into()]);
            } else {
                git_args.extend(["branch".into(), name.into()]);
            }
            if let Some(start) = args.get("start_point").and_then(Value::as_str) {
                git_args.push(validate_git_ref(start)?.to_string());
            }
        }
        "switch" => {
            git_args.extend(["switch".into(), name.into()]);
        }
        "delete" => {
            if !args
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                return Err(WorkspaceError::ToolDetails {
                    code: "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
                    message: "Deleting a branch requires confirm=true".into(),
                    category: "permission",
                    retryable: false,
                    details: json!({"action": action, "name": name}),
                });
            }
            git_args.extend([
                "branch".into(),
                if force { "-D".into() } else { "-d".into() },
                name.into(),
            ]);
        }
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "action must be create, switch, or delete",
            ))
        }
    }
    command.extend(git_args.clone());
    if dry_run {
        return Ok(tool_ok(json!({
            "dry_run": true,
            "applied": false,
            "action": action,
            "name": name,
            "command": command,
            "repo": target.metadata(),
            "warnings": []
        })));
    }
    let completed = run_git_strings(&target.root, &git_args, Duration::from_secs(15))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let status = git_status(ws, &json!({"path": target.repo_path}))?;
    Ok(tool_ok(json!({
        "dry_run": false,
        "applied": true,
        "action": action,
        "name": name,
        "command": command,
        "stdout": completed.stdout.trim(),
        "repo": target.metadata(),
        "status": status,
        "affected_files": [],
        "warnings": []
    })))
}

pub fn git_stage(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    prevalidate_git_paths(ws, args)?;
    let target = resolve_git_target(ws, args)?;
    verify_expected_head(&target, args)?;
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let paths = git_paths(ws, &target, args)?;
    if !all && paths.is_empty() {
        return Err(WorkspaceError::invalid_argument(
            "paths is required unless all=true",
        ));
    }
    let mut git_args = vec!["add".to_string()];
    if all {
        git_args.push("-A".into());
    } else {
        git_args.push("--".into());
        git_args.extend(paths.clone());
    }
    if args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(tool_ok(json!({
            "dry_run": true,
            "applied": false,
            "paths": paths,
            "all": all,
            "command": std::iter::once("git".to_string()).chain(git_args).collect::<Vec<_>>(),
            "repo": target.metadata(),
            "warnings": []
        })));
    }
    let completed = run_git_strings(&target.root, &git_args, Duration::from_secs(20))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let status = git_status(ws, &json!({"path": target.repo_path}))?;
    Ok(tool_ok(json!({
        "dry_run": false,
        "applied": true,
        "paths": paths,
        "all": all,
        "repo": target.metadata(),
        "status": status,
        "affected_files": paths.iter().map(|path| json!({"path": path, "operation": "stage"})).collect::<Vec<_>>(),
        "warnings": []
    })))
}

pub fn git_commit(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    prevalidate_git_paths(ws, args)?;
    let target = resolve_git_target(ws, args)?;
    verify_expected_head(&target, args)?;
    let message = args
        .get("message")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("message is required"))?;
    if message.trim().is_empty() || message.len() > 10_000 {
        return Err(WorkspaceError::invalid_argument(
            "message must be between 1 and 10000 characters",
        ));
    }
    let paths = git_paths(ws, &target, args)?;
    let all = args.get("all").and_then(Value::as_bool).unwrap_or(false);
    let allow_empty = args
        .get("allow_empty")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let require_clean_index = args
        .get("require_clean_index_before")
        .and_then(Value::as_bool)
        .unwrap_or(!paths.is_empty() || all);
    let index_clean = git_index_clean(&target.root)?;
    if require_clean_index && !index_clean {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_INDEX_NOT_CLEAN",
            message: "git_commit requires a clean index before staging paths".into(),
            category: "conflict",
            retryable: false,
            details: json!({"suggestion": "commit or unstage existing staged changes, or set require_clean_index_before=false"}),
        });
    }
    if args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(tool_ok(json!({
            "dry_run": true,
            "applied": false,
            "message": message,
            "paths": paths,
            "all": all,
            "index_clean": index_clean,
            "repo": target.metadata(),
            "warnings": []
        })));
    }

    let old_head = git_rev_parse(&target.root, "HEAD").unwrap_or_default();
    let staged_by_tool = !paths.is_empty() || all;
    if staged_by_tool {
        let mut add_args = vec!["add".to_string()];
        if all {
            add_args.push("-A".into());
        } else {
            add_args.push("--".into());
            add_args.extend(paths.clone());
        }
        let staged = run_git_strings(&target.root, &add_args, Duration::from_secs(20))?;
        if !staged.success {
            return Err(git_error(&staged.stderr));
        }
    }

    let mut commit_args = vec!["commit".to_string(), "-m".to_string(), message.to_string()];
    if allow_empty {
        commit_args.push("--allow-empty".into());
    }
    let committed = run_git_strings(&target.root, &commit_args, Duration::from_secs(60))?;
    if !committed.success {
        if staged_by_tool && index_clean {
            let _ = run_git(
                &target.root,
                &["reset", "--quiet", "HEAD", "--"],
                Duration::from_secs(10),
            );
        }
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_COMMIT_FAILED",
            message: committed.stderr.trim().to_string(),
            category: "runtime",
            retryable: false,
            details: json!({
                "stdout": committed.stdout,
                "staged_by_tool": staged_by_tool,
                "index_restored": staged_by_tool && index_clean
            }),
        });
    }
    let new_head = git_rev_parse(&target.root, "HEAD").unwrap_or_default();
    let status = git_status(ws, &json!({"path": target.repo_path}))?;
    Ok(tool_ok(json!({
        "dry_run": false,
        "applied": true,
        "commit": new_head,
        "previous_head": old_head,
        "message": message,
        "paths": paths,
        "all": all,
        "stdout": committed.stdout.trim(),
        "repo": target.metadata(),
        "status": status,
        "affected_files": paths.iter().map(|path| json!({"path": path, "operation": "commit"})).collect::<Vec<_>>(),
        "warnings": []
    })))
}

pub fn git_restore(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    prevalidate_git_paths(ws, args)?;
    let target = resolve_git_target(ws, args)?;
    verify_expected_head(&target, args)?;
    if !args
        .get("confirm")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Err(WorkspaceError::ToolDetails {
            code: "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION",
            message: "git_restore discards or unstages changes and requires confirm=true".into(),
            category: "permission",
            retryable: false,
            details: json!({}),
        });
    }
    let paths = git_paths(ws, &target, args)?;
    if paths.is_empty() {
        return Err(WorkspaceError::invalid_argument("paths is required"));
    }
    let staged = args.get("staged").and_then(Value::as_bool).unwrap_or(false);
    let worktree = args
        .get("worktree")
        .and_then(Value::as_bool)
        .unwrap_or(!staged);
    if !staged && !worktree {
        return Err(WorkspaceError::invalid_argument(
            "staged or worktree must be true",
        ));
    }
    let mut git_args = vec!["restore".to_string()];
    if staged {
        git_args.push("--staged".into());
    }
    if worktree {
        git_args.push("--worktree".into());
    }
    if let Some(source) = args.get("source").and_then(Value::as_str) {
        git_args.push(format!("--source={}", validate_git_ref(source)?));
    }
    git_args.push("--".into());
    git_args.extend(paths.clone());
    if args
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return Ok(tool_ok(json!({
            "dry_run": true,
            "applied": false,
            "paths": paths,
            "staged": staged,
            "worktree": worktree,
            "repo": target.metadata(),
            "warnings": []
        })));
    }
    let completed = run_git_strings(&target.root, &git_args, Duration::from_secs(20))?;
    if !completed.success {
        return Err(git_error(&completed.stderr));
    }
    let status = git_status(ws, &json!({"path": target.repo_path}))?;
    Ok(tool_ok(json!({
        "dry_run": false,
        "applied": true,
        "paths": paths,
        "staged": staged,
        "worktree": worktree,
        "repo": target.metadata(),
        "status": status,
        "affected_files": paths.iter().map(|path| json!({"path": path, "operation": "restore"})).collect::<Vec<_>>(),
        "warnings": []
    })))
}

fn resolve_git_target(ws: &Workspace, args: &Value) -> Result<GitTarget, WorkspaceError> {
    let repo_path = args.get("repo_path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_existing(repo_path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "repo_path must be a directory",
        ));
    }
    let root = git_value(&resolved.path, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .ok_or_else(|| WorkspaceError::Tool {
            code: "NOT_GIT_REPOSITORY",
            message: "repo_path is not inside a Git repository".into(),
            category: "validation",
            retryable: false,
        })?;
    let git_dir = git_value(&resolved.path, &["rev-parse", "--absolute-git-dir"])
        .unwrap_or_else(|| "missing".into());
    let common_dir = git_value(&resolved.path, &["rev-parse", "--git-common-dir"])
        .unwrap_or_else(|| git_dir.clone());
    let branch = git_value(&resolved.path, &["rev-parse", "--abbrev-ref", "HEAD"])
        .unwrap_or_else(|| "HEAD".into());
    let head = git_rev_parse(&resolved.path, "HEAD").unwrap_or_else(|| "missing".into());
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(
            [
                root.to_string_lossy().as_ref(),
                git_dir.as_str(),
                common_dir.as_str(),
                branch.as_str(),
                head.as_str(),
            ]
            .join("\0")
            .as_bytes(),
        )
    );
    let target = GitTarget {
        repo_path: repo_path.to_string(),
        root,
        git_dir,
        common_dir,
        branch,
        head,
        fingerprint,
    };
    if let Some(expected) = args
        .get("expected_repo_fingerprint")
        .and_then(Value::as_str)
    {
        if !expected.eq_ignore_ascii_case(&target.fingerprint) {
            return Err(WorkspaceError::ToolDetails {
                code: "GIT_REPO_TARGET_MISMATCH",
                message: "Git repository/worktree target changed since preflight".into(),
                category: "conflict",
                retryable: true,
                details: json!({
                    "expected_repo_fingerprint": expected,
                    "actual_repo_fingerprint": target.fingerprint,
                    "repo": target.metadata(),
                    "suggestion": "Call git_status with path=repo_path and retry with the returned repo_fingerprint"
                }),
            });
        }
    }
    Ok(target)
}

fn verify_expected_head(target: &GitTarget, args: &Value) -> Result<(), WorkspaceError> {
    if let Some(expected) = args.get("expected_head").and_then(Value::as_str) {
        let actual = &target.head;
        if !expected.eq_ignore_ascii_case(&actual) {
            return Err(WorkspaceError::ToolDetails {
                code: "GIT_HEAD_MISMATCH",
                message: "Git HEAD changed since preflight".into(),
                category: "conflict",
                retryable: true,
                details: json!({
                    "expected_head": expected,
                    "actual_head": actual,
                    "repo": target.metadata()
                }),
            });
        }
    }
    Ok(())
}

fn validate_branch_name(root: &Path, name: &str) -> Result<(), WorkspaceError> {
    let completed = run_git(
        root,
        &["check-ref-format", "--branch", name],
        Duration::from_secs(5),
    )?;
    if completed.success {
        Ok(())
    } else {
        Err(WorkspaceError::invalid_argument(format!(
            "Invalid branch name: {name}"
        )))
    }
}

fn git_paths(
    ws: &Workspace,
    target: &GitTarget,
    args: &Value,
) -> Result<Vec<String>, WorkspaceError> {
    let mut paths = Vec::new();
    if let Some(items) = args.get("paths").and_then(Value::as_array) {
        for item in items {
            let path = item
                .as_str()
                .ok_or_else(|| WorkspaceError::invalid_argument("paths entries must be strings"))?;
            ws.reject_unsafe_text(path)?;
            let workspace_path = if target.repo_path == "." {
                path.to_string()
            } else {
                format!(
                    "{}/{}",
                    target.repo_path.trim_end_matches(['/', '\\']),
                    path
                )
            };
            ws.reject_protected_write_path(&workspace_path)?;
            paths.push(path.to_string());
        }
    }
    Ok(paths)
}

fn prevalidate_git_paths(ws: &Workspace, args: &Value) -> Result<(), WorkspaceError> {
    if let Some(items) = args.get("paths").and_then(Value::as_array) {
        for item in items {
            let path = item
                .as_str()
                .ok_or_else(|| WorkspaceError::invalid_argument("paths entries must be strings"))?;
            ws.reject_unsafe_text(path)?;
            ws.reject_protected_write_path(path)?;
        }
    }
    Ok(())
}

fn git_index_clean(root: &Path) -> Result<bool, WorkspaceError> {
    let completed = run_git(
        root,
        &["diff", "--cached", "--quiet", "--exit-code"],
        Duration::from_secs(10),
    )?;
    match completed.exit_code {
        0 => Ok(true),
        1 => Ok(false),
        _ => Err(git_error(&completed.stderr)),
    }
}

fn run_git_strings(
    cwd: &std::path::Path,
    args: &[String],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_git(cwd, &refs, limit)
}

fn validate_git_ref(ref_name: &str) -> Result<&str, WorkspaceError> {
    if ref_name.is_empty()
        || ref_name.starts_with('-')
        || ref_name.contains('\0')
        || ref_name.contains('\n')
        || ref_name.contains('\r')
    {
        return Err(WorkspaceError::invalid_argument("Invalid git revision."));
    }
    Ok(ref_name)
}

fn parse_git_blame_porcelain(output: &str) -> Vec<Value> {
    let commit_re = Regex::new(r"^[0-9a-fA-F^]{40}").expect("valid regex");
    let mut rows = Vec::new();
    let mut current: serde_json::Map<String, Value> = serde_json::Map::new();

    for raw in output.lines() {
        let parts: Vec<&str> = raw.split_whitespace().collect();
        if parts.len() >= 3 && commit_re.is_match(parts[0]) {
            current = serde_json::Map::new();
            current.insert("commit".into(), json!(parts[0].trim_start_matches('^')));
            if parts[1].chars().all(|c| c.is_ascii_digit()) {
                current.insert("original_line".into(), json!(parts[1].parse::<i64>().ok()));
            }
            if parts[2].chars().all(|c| c.is_ascii_digit()) {
                current.insert("line".into(), json!(parts[2].parse::<i64>().ok()));
            }
            continue;
        }
        if let Some(author) = raw.strip_prefix("author ") {
            current.insert("author".into(), json!(author));
            continue;
        }
        if let Some(mail) = raw.strip_prefix("author-mail ") {
            current.insert(
                "author_mail".into(),
                json!(mail.trim_matches(|c| c == '<' || c == '>')),
            );
            continue;
        }
        if let Some(time) = raw.strip_prefix("author-time ") {
            let value = if time.chars().all(|c| c.is_ascii_digit()) {
                json!(time.parse::<i64>().ok())
            } else {
                json!(time)
            };
            current.insert("author_time".into(), value);
            continue;
        }
        if let Some(summary) = raw.strip_prefix("summary ") {
            current.insert("summary".into(), json!(summary));
            continue;
        }
        if let Some(content) = raw.strip_prefix('\t') {
            let mut row = current.clone();
            row.insert("content".into(), json!(content));
            rows.push(Value::Object(row));
        }
    }
    rows
}

struct GitOutput {
    success: bool,
    exit_code: i32,
    stdout: String,
    stderr: String,
}

fn read_child_pipe<R>(mut reader: R) -> thread::JoinHandle<io::Result<Vec<u8>>>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

fn join_child_pipe(reader: Option<thread::JoinHandle<io::Result<Vec<u8>>>>) -> io::Result<Vec<u8>> {
    match reader {
        Some(reader) => reader
            .join()
            .map_err(|_| io::Error::other("Git output reader panicked"))?,
        None => Ok(Vec::new()),
    }
}

fn wait_for_child_output(mut child: Child, limit: Duration) -> io::Result<(Output, bool)> {
    #[cfg(windows)]
    let _process_tree_guard = crate::platform::attach_process_tree(child.id());
    let stdout = child.stdout.take().map(read_child_pipe);
    let stderr = child.stderr.take().map(read_child_pipe);
    let started = Instant::now();
    let (status, timed_out) = loop {
        match child.try_wait() {
            Ok(Some(status)) => break (status, false),
            Ok(None) if started.elapsed() < limit => thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                #[cfg(windows)]
                {
                    let _ = crate::platform::platform().terminate_process_tree(child.id());
                }
                let _ = child.kill();
                break (child.wait()?, true);
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    };
    Ok((
        Output {
            status,
            stdout: join_child_pipe(stdout)?,
            stderr: join_child_pipe(stderr)?,
        },
        timed_out,
    ))
}

fn run_git(
    cwd: &std::path::Path,
    args: &[&str],
    limit: Duration,
) -> Result<GitOutput, WorkspaceError> {
    let command_args = args
        .iter()
        .map(|arg| (*arg).to_string())
        .collect::<Vec<_>>();
    let mut cmd = std_command_for_workspace("git", &command_args, cwd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0");

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let child = cmd
        .spawn()
        .map_err(|e| git_error(&format!("git not available: {e}")))?;
    let (output, timed_out) = wait_for_child_output(child, limit)
        .map_err(|error| git_error(&format!("git process failed: {error}")))?;
    if timed_out {
        return Err(WorkspaceError::ToolDetails {
            code: "GIT_COMMAND_TIMEOUT",
            message: format!("Git command exceeded {} ms", limit.as_millis()),
            category: "timeout",
            retryable: true,
            details: json!({
                "timeout_ms": limit.as_millis(),
                "stderr": String::from_utf8_lossy(&output.stderr).trim()
            }),
        });
    }
    Ok(GitOutput {
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

fn run_git_diff(
    root: &std::path::Path,
    context: u64,
    path_filters: &[String],
    cached: bool,
) -> Result<String, WorkspaceError> {
    let unified = format!("--unified={context}");
    let mut args = vec!["diff", unified.as_str()];
    if cached {
        args.push("--cached");
    }
    if !path_filters.is_empty() {
        args.push("--");
        for p in path_filters {
            args.push(p.as_str());
        }
    }
    let completed = run_git(root, &args, Duration::from_secs(10))?;
    if completed.exit_code != 0 && completed.exit_code != 1 {
        return Err(git_error(&completed.stderr));
    }
    Ok(completed.stdout)
}

fn is_git_repo(root: &std::path::Path) -> bool {
    run_git(root, &["rev-parse", "--git-dir"], Duration::from_secs(5))
        .map(|o| o.success)
        .unwrap_or(false)
}

fn git_rev_parse(cwd: &std::path::Path, rev: &str) -> Option<String> {
    run_git(cwd, &["rev-parse", rev], Duration::from_secs(5))
        .ok()
        .filter(|o| o.success)
        .map(|o| o.stdout.trim().to_string())
}

fn git_value(cwd: &Path, args: &[&str]) -> Option<String> {
    run_git(cwd, args, Duration::from_secs(5))
        .ok()
        .filter(|output| output.success)
        .map(|output| output.stdout.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_branch_line(line: &str) -> (String, String, i64, i64) {
    let (branch_part, tracking) = line
        .split_once("...")
        .map(|(b, t)| (b.to_string(), t.to_string()))
        .unwrap_or((line.to_string(), String::new()));
    let branch = branch_part
        .split_once(' ')
        .map(|(b, _)| b.to_string())
        .unwrap_or(branch_part);
    let mut ahead = 0i64;
    let mut behind = 0i64;
    let mut upstream = tracking.clone();
    if let Some(idx) = tracking.find(' ') {
        upstream = tracking[..idx].to_string();
        let meta = &tracking[idx + 1..];
        for token in meta.split(',') {
            let token = token.trim();
            if let Some(n) = token.strip_prefix("ahead ") {
                ahead = n.trim().parse().unwrap_or(0);
            } else if let Some(n) = token.strip_prefix("behind ") {
                behind = n.trim().parse().unwrap_or(0);
            }
        }
    }
    (branch, upstream, ahead, behind)
}

fn parse_diff_files(diff: &str) -> Vec<Value> {
    let mut files = Vec::new();
    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            files.push(json!({
                "path": path,
                "status": "modified",
                "binary": false
            }));
        } else if line.starts_with("--- /dev/null") {
            continue;
        } else if let Some(path) = line.strip_prefix("--- a/") {
            if !files.iter().any(|f| f["path"] == path) {
                files.push(json!({
                    "path": path,
                    "status": "modified",
                    "binary": false
                }));
            }
        }
    }
    files
}

fn git_error(message: &str) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "GIT_ERROR",
        message: message.to_string(),
        category: "runtime",
        retryable: false,
    }
}

#[cfg(test)]
mod tests {
    use super::wait_for_child_output;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    fn child_output_timeout_is_enforced() {
        #[cfg(windows)]
        let mut command = {
            let mut command = Command::new("cmd");
            command.args(["/d", "/c", "ping 127.0.0.1 -n 3 >nul"]);
            command
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut command = Command::new("sh");
            command.args(["-c", "sleep 1"]);
            command
        };
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().expect("spawn timeout test child");
        let started = Instant::now();
        let (_, timed_out) = wait_for_child_output(child, Duration::from_millis(50))
            .expect("collect timed out child");
        assert!(timed_out);
        assert!(started.elapsed() < Duration::from_secs(2));
    }
}
