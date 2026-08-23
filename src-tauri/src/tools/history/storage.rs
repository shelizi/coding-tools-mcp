use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::tools::workspace::{relative_display, Workspace, WorkspaceError, WorkspaceResult};

use super::markdown;
use super::model::{HistoryDocument, HistoryIndex, IndexEntry, ScanReport};

pub const DEFAULT_HISTORY_DIR: &str = "docs/history-session";

const HISTORY_LOCK_DIR: &str = ".history.lock.d";
const HISTORY_LOCK_OWNER_FILE: &str = "owner.json";
const HISTORY_LOCK_TIMEOUT: Duration = Duration::from_secs(5);
const HISTORY_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(10);
const HISTORY_LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
const MAX_INDEX_SUMMARY_CHARS: usize = 3_000;
const MAX_SUMMARY_PREFIX_BYTES: u64 = 256 * 1024;

pub struct HistorySummaryRead {
    pub summary: String,
    pub session_key: Option<String>,
    pub bytes_read: u64,
    pub content_bytes: u64,
}

pub struct HistoryLock {
    path: PathBuf,
    token: String,
    wait_ms: u128,
}

impl HistoryLock {
    pub fn wait_ms(&self) -> u128 {
        self.wait_ms
    }
}

impl Drop for HistoryLock {
    fn drop(&mut self) {
        let owner_path = self.path.join(HISTORY_LOCK_OWNER_FILE);
        let owned = fs::read_to_string(owner_path)
            .ok()
            .and_then(|content| serde_json::from_str::<serde_json::Value>(&content).ok())
            .and_then(|value| {
                value
                    .get("token")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .is_some_and(|token| token == self.token);
        if owned {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

pub fn resolve_history_dir(
    workspace: &Workspace,
    workspace_root: Option<&str>,
    history_dir: Option<&str>,
) -> WorkspaceResult<PathBuf> {
    if let Some(requested_root) = workspace_root {
        let requested_path = Path::new(requested_root.trim());
        let candidate = if requested_path.is_absolute() {
            requested_path.to_path_buf()
        } else {
            workspace.root().join(requested_path)
        };
        let requested = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::invalid_argument("workspace_root does not exist"))?;
        if requested != workspace.root() {
            return Err(WorkspaceError::path_outside_workspace());
        }
    }

    let raw = history_dir.unwrap_or(DEFAULT_HISTORY_DIR).trim();
    if raw.is_empty() || workspace.reject_unsafe_text(raw).is_err() {
        return Err(WorkspaceError::path_outside_workspace());
    }
    let candidate = workspace
        .root()
        .join(raw.replace('/', std::path::MAIN_SEPARATOR_STR));
    ensure_safe_candidate(workspace, &candidate)?;
    if candidate.exists() && !candidate.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "history_dir must be a directory",
        ));
    }
    Ok(candidate)
}

fn ensure_safe_candidate(workspace: &Workspace, candidate: &Path) -> WorkspaceResult<()> {
    if candidate.exists() || candidate.is_symlink() {
        let resolved = candidate
            .canonicalize()
            .map_err(|_| WorkspaceError::path_outside_workspace())?;
        if !resolved.starts_with(workspace.root()) {
            return Err(WorkspaceError::path_outside_workspace());
        }
        return Ok(());
    }
    let mut ancestor = candidate.parent();
    while let Some(path) = ancestor {
        if path.exists() || path.is_symlink() {
            let resolved = path
                .canonicalize()
                .map_err(|_| WorkspaceError::path_outside_workspace())?;
            if !resolved.starts_with(workspace.root()) {
                return Err(WorkspaceError::path_outside_workspace());
            }
            return Ok(());
        }
        ancestor = path.parent();
    }
    Err(WorkspaceError::path_outside_workspace())
}

pub fn ensure_directory(path: &Path) -> WorkspaceResult<()> {
    fs::create_dir_all(path).map_err(|error| io_error("HISTORY_WRITE_FAILED", error, true))
}

pub fn lock_directory(path: &Path) -> WorkspaceResult<HistoryLock> {
    ensure_directory(path)?;
    let lock_path = path.join(HISTORY_LOCK_DIR);
    let token = uuid::Uuid::new_v4().to_string();
    let started = Instant::now();
    loop {
        match fs::create_dir(&lock_path) {
            Ok(()) => {
                let created_at_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis();
                let owner = serde_json::json!({
                    "version": 1,
                    "token": token,
                    "pid": std::process::id(),
                    "created_at_ms": created_at_ms
                });
                if let Err(error) = fs::write(
                    lock_path.join(HISTORY_LOCK_OWNER_FILE),
                    serde_json::to_vec(&owner).unwrap_or_default(),
                ) {
                    let _ = fs::remove_dir_all(&lock_path);
                    return Err(io_error("HISTORY_LOCK_FAILED", error, true));
                }
                return Ok(HistoryLock {
                    path: lock_path,
                    token,
                    wait_ms: started.elapsed().as_millis(),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if lock_directory_is_stale(&lock_path) {
                    let _ = fs::remove_dir_all(&lock_path);
                    continue;
                }
                if started.elapsed() >= HISTORY_LOCK_TIMEOUT {
                    return Err(WorkspaceError::ToolDetails {
                        code: "HISTORY_LOCK_TIMEOUT",
                        message: "Timed out waiting for the history archive lock.".into(),
                        category: "runtime",
                        retryable: true,
                        details: serde_json::json!({
                            "history_lock_wait_ms": started.elapsed().as_millis(),
                            "timeout_ms": HISTORY_LOCK_TIMEOUT.as_millis(),
                            "suggestion": "Retry after the current history write completes"
                        }),
                    });
                }
                thread::sleep(HISTORY_LOCK_RETRY_INTERVAL);
            }
            Err(error) => return Err(io_error("HISTORY_LOCK_FAILED", error, true)),
        }
    }
}

fn lock_directory_is_stale(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .and_then(|modified| modified.elapsed().map_err(io::Error::other))
        .is_ok_and(|elapsed| elapsed >= HISTORY_LOCK_STALE_AFTER)
}

fn indexed_document_path(
    workspace: &Workspace,
    history_dir: &Path,
    entry: &IndexEntry,
) -> WorkspaceResult<PathBuf> {
    ensure_safe_candidate(workspace, history_dir)?;
    let path = history_dir.join(format!("{}.md", entry.number));
    ensure_safe_candidate(workspace, &path)?;
    let resolved_path = relative_display(workspace.root(), &path);
    if resolved_path != entry.path {
        return Err(WorkspaceError::ToolDetails {
            code: "HISTORY_INDEX_STALE",
            message: "History index path does not match its numbered Markdown file.".into(),
            category: "validation",
            retryable: true,
            details: serde_json::json!({
                "indexed_path": entry.path,
                "resolved_path": resolved_path,
                "number": entry.number
            }),
        });
    }
    Ok(path)
}

pub fn read_document(
    workspace: &Workspace,
    history_dir: &Path,
    entry: &IndexEntry,
) -> WorkspaceResult<HistoryDocument> {
    let path = indexed_document_path(workspace, history_dir, entry)?;
    let bytes = fs::read(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
    let content = String::from_utf8(bytes).map_err(|error| WorkspaceError::ToolDetails {
        code: "HISTORY_INVALID_UTF8",
        message: "History Markdown must be UTF-8.".into(),
        category: "validation",
        retryable: false,
        details: serde_json::json!({"file": entry.path, "error": error.to_string()}),
    })?;
    Ok(HistoryDocument {
        number: entry.number,
        path: entry.path.clone(),
        session_key: markdown::metadata(&content, "Session key"),
        created_at: markdown::metadata(&content, "Created"),
        updated_at: markdown::metadata(&content, "Updated"),
        content,
    })
}

pub fn read_summary(
    workspace: &Workspace,
    history_dir: &Path,
    entry: &IndexEntry,
) -> WorkspaceResult<HistorySummaryRead> {
    let path = indexed_document_path(workspace, history_dir, entry)?;
    let content_bytes = fs::metadata(&path)
        .map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?
        .len();
    let file = File::open(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
    let mut reader = io::BufReader::new(file);
    let mut prefix = String::new();
    let mut bytes_read = 0_u64;
    loop {
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        if read == 0 {
            break;
        }
        bytes_read = bytes_read.saturating_add(read as u64);
        if line.trim() == "## 本轮检查点" {
            break;
        }
        prefix.push_str(&line);
        if bytes_read >= MAX_SUMMARY_PREFIX_BYTES {
            break;
        }
    }
    Ok(HistorySummaryRead {
        summary: bounded_summary(&prefix),
        session_key: markdown::metadata(&prefix, "Session key"),
        bytes_read,
        content_bytes,
    })
}

pub fn update_index_entry_cache(entry: &mut IndexEntry, content: &str) -> bool {
    let summary = bounded_summary(content);
    let content_sha256 = sha256(content.as_bytes());
    let content_bytes = content.len() as u64;
    let changed = entry.summary != summary
        || entry.content_sha256 != content_sha256
        || entry.content_bytes != content_bytes;
    entry.summary = summary;
    entry.content_sha256 = content_sha256;
    entry.content_bytes = content_bytes;
    changed
}

fn bounded_summary(content: &str) -> String {
    let summary = markdown::summary(content);
    if summary.chars().count() <= MAX_INDEX_SUMMARY_CHARS {
        return summary;
    }
    let mut bounded = summary
        .chars()
        .take(MAX_INDEX_SUMMARY_CHARS)
        .collect::<String>();
    bounded.push_str("…（摘要已截断）");
    bounded
}

pub fn scan(workspace: &Workspace, history_dir: &Path) -> WorkspaceResult<ScanReport> {
    if !history_dir.exists() {
        return Ok(ScanReport::default());
    }
    ensure_safe_candidate(workspace, history_dir)?;
    let mut report = ScanReport::default();
    let entries =
        fs::read_dir(history_dir).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
    for entry in entries {
        let entry = entry.map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        let file_type = entry
            .file_type()
            .map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        if !file_type.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "README.md" | "index.json" | ".history.lock")
            || name.starts_with(".history-tmp-")
        {
            continue;
        }
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            report.invalid_files.push(name);
            continue;
        };
        let is_markdown = path.extension().and_then(|value| value.to_str()) == Some("md");
        let number = stem.parse::<u64>().ok();
        if !is_markdown
            || number.is_none()
            || number == Some(0)
            || number.map(|value| value.to_string()) != Some(stem.to_string())
        {
            report.invalid_files.push(name);
            continue;
        }
        let number = number.expect("validated number");
        let bytes =
            fs::read(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
        let content = String::from_utf8(bytes).map_err(|error| WorkspaceError::ToolDetails {
            code: "HISTORY_INVALID_UTF8",
            message: "History Markdown must be UTF-8.".into(),
            category: "validation",
            retryable: false,
            details: serde_json::json!({"file": name, "error": error.to_string()}),
        })?;
        if content.trim().is_empty() {
            report.empty_files.push(name.clone());
        }
        report.documents.push(HistoryDocument {
            number,
            path: relative_display(workspace.root(), &path),
            session_key: markdown::metadata(&content, "Session key"),
            created_at: markdown::metadata(&content, "Created"),
            updated_at: markdown::metadata(&content, "Updated"),
            content,
        });
    }
    report.documents.sort_by_key(|document| document.number);
    report.invalid_files.sort();
    report.empty_files.sort();
    report.numbers = report
        .documents
        .iter()
        .map(|document| document.number)
        .collect();
    if let Some(latest) = report.latest_number() {
        let present = report.numbers.iter().copied().collect::<BTreeSet<_>>();
        report.missing_numbers = (1..=latest)
            .filter(|number| !present.contains(number))
            .collect();
    }
    let mut keys = BTreeMap::<String, usize>::new();
    for key in report
        .documents
        .iter()
        .filter_map(|document| document.session_key.as_ref())
    {
        *keys.entry(key.clone()).or_default() += 1;
    }
    report.duplicate_session_keys = keys
        .into_iter()
        .filter_map(|(key, count)| (count > 1).then_some(key))
        .collect();
    Ok(report)
}

pub fn rebuild_index(report: &ScanReport) -> HistoryIndex {
    let duplicates = report
        .duplicate_session_keys
        .iter()
        .collect::<BTreeSet<_>>();
    let mut index = HistoryIndex {
        latest_number: report.latest_number().unwrap_or(0),
        ..HistoryIndex::default()
    };
    for document in &report.documents {
        let Some(session_key) = document.session_key.as_ref() else {
            continue;
        };
        if duplicates.contains(session_key) {
            continue;
        }
        index.sessions.insert(session_key.clone(), {
            let mut entry = IndexEntry {
                number: document.number,
                path: document.path.clone(),
                created_at: document.created_at.clone().unwrap_or_default(),
                updated_at: document.updated_at.clone().unwrap_or_default(),
                summary: String::new(),
                content_sha256: String::new(),
                content_bytes: 0,
            };
            update_index_entry_cache(&mut entry, &document.content);
            entry
        });
    }
    index
}

pub fn read_index(history_dir: &Path) -> WorkspaceResult<Option<HistoryIndex>> {
    let path = history_dir.join("index.json");
    if !path.exists() {
        return Ok(None);
    }
    let content =
        fs::read_to_string(&path).map_err(|error| io_error("HISTORY_READ_FAILED", error, true))?;
    serde_json::from_str(&content)
        .map(Some)
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "HISTORY_INDEX_INVALID",
            message: "History index is not valid JSON.".into(),
            category: "validation",
            retryable: true,
            details: serde_json::json!({"error": error.to_string()}),
        })
}

pub fn write_index(history_dir: &Path, index: &HistoryIndex) -> WorkspaceResult<()> {
    let content =
        serde_json::to_vec_pretty(index).map_err(|error| WorkspaceError::ToolDetails {
            code: "HISTORY_WRITE_FAILED",
            message: "Unable to serialize history index.".into(),
            category: "internal",
            retryable: true,
            details: serde_json::json!({"error": error.to_string()}),
        })?;
    atomic_write(&history_dir.join("index.json"), &content)
}

pub fn write_markdown(path: &Path, content: &str) -> WorkspaceResult<()> {
    atomic_write(path, content.as_bytes())
}

pub fn sha256(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

fn atomic_write(target: &Path, content: &[u8]) -> WorkspaceResult<()> {
    let parent = target
        .parent()
        .ok_or_else(|| WorkspaceError::invalid_argument("History target has no parent"))?;
    ensure_directory(parent)?;
    let temp = parent.join(format!(".history-tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        atomic_replace(&temp, target)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(|error| io_error("HISTORY_WRITE_FAILED", error, true))
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn atomic_replace(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    unsafe {
        MoveFileExW(
            PCWSTR(source.as_ptr()),
            PCWSTR(target.as_ptr()),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
        .map_err(|error| io::Error::other(error.to_string()))
    }
}

fn io_error(code: &'static str, error: io::Error, retryable: bool) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: error.to_string(),
        category: "filesystem",
        retryable,
        details: serde_json::json!({"kind": format!("{:?}", error.kind())}),
    }
}
