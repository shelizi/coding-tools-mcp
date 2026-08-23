use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use ignore::WalkBuilder;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use similar::TextDiff;
use uuid::Uuid;

use super::{relative_path, ActionPlan, ActionRequest};
use crate::tools::workspace::{Workspace, WorkspaceError};

#[derive(Clone)]
pub(super) struct OriginalFile {
    pub(super) bytes: Vec<u8>,
    sha256: String,
}

pub(super) struct MirrorGuard {
    pub(super) root: PathBuf,
    parent: PathBuf,
}

impl MirrorGuard {
    pub(super) fn create(workspace_root: &Path) -> Result<Self, WorkspaceError> {
        let parent = workspace_root.join(".coding-tools-format");
        let root = parent.join(Uuid::new_v4().to_string());
        fs::create_dir_all(&root).map_err(|error| WorkspaceError::ToolDetails {
            code: "FORMAT_MIRROR_CREATE_FAILED",
            message: "Could not create isolated formatter mirror".into(),
            category: "runtime",
            retryable: true,
            details: json!({"error": error.to_string()}),
        })?;
        Ok(Self { root, parent })
    }
}

impl Drop for MirrorGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
        let _ = fs::remove_dir(&self.parent);
    }
}

pub(super) fn read_originals(
    ws: &Workspace,
    plan: &ActionPlan,
    request: &ActionRequest,
) -> Result<BTreeMap<String, OriginalFile>, WorkspaceError> {
    let mut originals = BTreeMap::new();
    for file in &plan.files {
        let resolved = ws.resolve_existing(&file.path)?;
        let bytes = fs::read(&resolved.path)
            .map_err(|_| WorkspaceError::not_found(format!("File not found: {}", file.path)))?;
        let sha256 = sha256_hex(&bytes);
        if let Some(expected) = request.expected_sha256.get(&file.path) {
            if !expected.eq_ignore_ascii_case(&sha256) {
                return Err(version_mismatch(&file.path, expected, &sha256));
            }
        }
        originals.insert(file.path.clone(), OriginalFile { bytes, sha256 });
    }
    Ok(originals)
}

pub(super) fn prepare_mirror(
    ws: &Workspace,
    mirror_root: &Path,
    plan: &ActionPlan,
) -> Result<(), WorkspaceError> {
    let mut support_files = BTreeSet::new();
    for file in &plan.files {
        copy_workspace_file(ws, mirror_root, &file.path)?;
        if let Some(config) = file.config_path.as_deref() {
            support_files.insert(config.to_string());
        }
        collect_nearest_support_files(ws.root(), &file.path, &mut support_files);
    }
    for path in support_files {
        if ws.root().join(&path).is_file() {
            copy_workspace_file(ws, mirror_root, &path)?;
        }
    }
    Ok(())
}

fn collect_nearest_support_files(root: &Path, file: &str, files: &mut BTreeSet<String>) {
    const SUPPORT_NAMES: &[&str] = &[
        "package.json",
        "pyproject.toml",
        "Cargo.toml",
        "go.mod",
        ".editorconfig",
    ];
    let mut current = root.join(file).parent().map(Path::to_path_buf);
    while let Some(directory) = current {
        for name in SUPPORT_NAMES {
            let candidate = directory.join(name);
            if candidate.is_file() {
                files.insert(relative_path(root, &candidate));
            }
        }
        if directory == root {
            break;
        }
        current = directory.parent().map(Path::to_path_buf);
    }
}

fn copy_workspace_file(
    ws: &Workspace,
    mirror_root: &Path,
    path: &str,
) -> Result<(), WorkspaceError> {
    let source = ws.resolve_existing(path)?;
    if !source.path.is_file() {
        return Ok(());
    }
    let destination = mirror_root.join(path.replace('/', std::path::MAIN_SEPARATOR_STR));
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| WorkspaceError::ToolDetails {
            code: "FORMAT_MIRROR_COPY_FAILED",
            message: format!("Could not prepare mirror directory for {path}"),
            category: "runtime",
            retryable: true,
            details: json!({"path": path, "error": error.to_string()}),
        })?;
    }
    fs::copy(&source.path, &destination).map_err(|error| WorkspaceError::ToolDetails {
        code: "FORMAT_MIRROR_COPY_FAILED",
        message: format!("Could not copy {path} into formatter mirror"),
        category: "runtime",
        retryable: true,
        details: json!({"path": path, "error": error.to_string()}),
    })?;
    Ok(())
}

pub(super) fn format_json_file(path: &Path) -> Result<(), WorkspaceError> {
    let bytes = fs::read(path).map_err(|_| {
        WorkspaceError::not_found(format!("Mirror file not found: {}", path.display()))
    })?;
    let value: Value =
        serde_json::from_slice(&bytes).map_err(|error| WorkspaceError::ToolDetails {
            code: "FORMATTER_FAILED",
            message: format!("Invalid JSON: {}", path.display()),
            category: "validation",
            retryable: false,
            details: json!({"adapter_id": "builtin-json", "error": error.to_string()}),
        })?;
    let mut output =
        serde_json::to_string_pretty(&value).map_err(|error| WorkspaceError::ToolDetails {
            code: "FORMATTER_FAILED",
            message: format!("Could not format JSON: {}", path.display()),
            category: "runtime",
            retryable: false,
            details: json!({"adapter_id": "builtin-json", "error": error.to_string()}),
        })?;
    output.push('\n');
    fs::write(path, output).map_err(|error| WorkspaceError::ToolDetails {
        code: "FORMAT_MIRROR_WRITE_FAILED",
        message: format!("Could not write mirror file: {}", path.display()),
        category: "runtime",
        retryable: true,
        details: json!({"error": error.to_string()}),
    })
}

pub(super) fn snapshot_tree(root: &Path) -> Result<BTreeMap<String, String>, WorkspaceError> {
    let mut snapshot = BTreeMap::new();
    let mut builder = WalkBuilder::new(root);
    builder
        .follow_links(false)
        .hidden(false)
        .ignore(false)
        .git_ignore(false)
        .git_global(false)
        .git_exclude(false)
        .parents(false)
        .require_git(false);
    for entry in builder.build().filter_map(Result::ok) {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = entry.path();
        let display = relative_path(root, path);
        if is_ignored_mirror_artifact(&display) {
            continue;
        }
        let bytes = fs::read(path).map_err(|error| WorkspaceError::ToolDetails {
            code: "FORMAT_MIRROR_SNAPSHOT_FAILED",
            message: format!("Could not snapshot mirror file {display}"),
            category: "runtime",
            retryable: true,
            details: json!({"error": error.to_string()}),
        })?;
        snapshot.insert(display, sha256_hex(&bytes));
    }
    Ok(snapshot)
}

fn is_ignored_mirror_artifact(path: &str) -> bool {
    path.split('/').any(|part| {
        matches!(
            part,
            "node_modules" | "target" | ".git" | ".cache" | ".ruff_cache" | "__pycache__"
        )
    })
}

pub(super) fn changed_paths(
    before: &BTreeMap<String, String>,
    after: &BTreeMap<String, String>,
) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

pub(super) fn apply_guarded(
    ws: &Workspace,
    originals: &BTreeMap<String, OriginalFile>,
    formatted: &BTreeMap<String, Vec<u8>>,
) -> Result<(), WorkspaceError> {
    for path in formatted.keys() {
        let resolved = ws.resolve_existing(path)?;
        let current = fs::read(&resolved.path)
            .map_err(|_| WorkspaceError::not_found(format!("File not found: {path}")))?;
        let actual = sha256_hex(&current);
        let expected = &originals.get(path).expect("formatted original").sha256;
        if actual != *expected {
            return Err(version_mismatch(path, expected, &actual));
        }
    }

    let mut written: Vec<String> = Vec::new();
    for (path, bytes) in formatted {
        let resolved = ws.resolve_existing(path)?;
        if let Err(error) = fs::write(&resolved.path, bytes) {
            for rollback_path in written.iter().rev() {
                if let (Ok(rollback_resolved), Some(original)) = (
                    ws.resolve_existing(rollback_path),
                    originals.get(rollback_path),
                ) {
                    let _ = fs::write(rollback_resolved.path, &original.bytes);
                }
            }
            return Err(WorkspaceError::ToolDetails {
                code: "FORMAT_APPLY_FAILED",
                message: format!("Could not apply formatted output to {path}"),
                category: "runtime",
                retryable: true,
                details: json!({"path": path, "error": error.to_string(), "rolled_back": written}),
            });
        }
        written.push(path.clone());
    }
    Ok(())
}

pub(super) fn unified_diff(path: &str, original: &str, updated: &str) -> String {
    TextDiff::from_lines(original, updated)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string()
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
            "suggestion": "Read the file again and rebuild the formatting request"
        }),
    }
}

pub(super) fn bounded_text(value: &str, max_bytes: usize) -> String {
    truncate_text(value.to_string(), max_bytes).0
}

pub(super) fn truncate_text(value: String, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value, false);
    }
    let mut boundary = max_bytes.min(value.len());
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    (value[..boundary].to_string(), true)
}
