use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const P0_TOOLS: &[(&str, &str, &str, bool, bool, bool)] = &[
    (
        "harness_status",
        "Harness status",
        "Return durable task, workspace, capability, and recovery status.",
        true,
        false,
        false,
    ),
    (
        "operation_log",
        "Operation log",
        "Return Workspace-level operation history independent of Task state.",
        true,
        false,
        false,
    ),
    (
        "server_info",
        "Server info",
        "Return server, workspace, auth, profile, and exposed-tool metadata.",
        true,
        false,
        false,
    ),
    (
        "list_workspace_folders",
        "List tool hub folders",
        "List folders configured in this tool hub and show the selected or default routing context.",
        true,
        false,
        false,
    ),
    (
        "switch_workspace_folder",
        "Switch conversation folder",
        "Switch the current ChatGPT conversation to one allowed folder without reconnecting the shared MCP. Each folder keeps independent workspace history.",
        false,
        false,
        false,
    ),
    (
        "query_tool_usage",
        "Query tool usage",
        "Safely query complete MCP tool-usage JSONL records and aggregate errors, server latency, client orchestration gaps, async child-process lifetimes, activity bursts, traffic, and warnings without reading a partial writer tail.",
        true,
        false,
        false,
    ),
    (
        "history_session_bootstrap",
        "Initialize or restore development session",
        "At the start of every new ChatGPT conversation, call this exactly once before the first response, even when the user did not ask to restore. It creates the first history session when none exists, or returns ordered summaries plus the latest full handoff and resumes the current ChatGPT session without duplicates.",
        false,
        false,
        false,
    ),
    (
        "history_session_checkpoint",
        "Save development checkpoint",
        "Save or update one idempotent, redacted development handoff. Pass session_key and expected_path exactly as returned by history_session_bootstrap so changing host metadata cannot redirect the checkpoint. The turn_id is optional and generated deterministically when omitted.",
        false,
        false,
        false,
    ),
    (
        "history_session_validate",
        "Validate session archive",
        "Validate history numbering, files, session mappings, and optionally rebuild the derived index without deleting history.",
        false,
        false,
        false,
    ),
    (
        "project_state",
        "Project state",
        "Return the current project, task, change, and verification state.",
        true,
        false,
        false,
    ),
    (
        "start_task",
        "Start task",
        "Start a durable coding task and capture the workspace baseline.",
        false,
        false,
        false,
    ),
    (
        "update_task",
        "Update task",
        "Update task steps and durable progress.",
        false,
        false,
        false,
    ),
    (
        "pause_task",
        "Pause task",
        "Pause the active coding task.",
        false,
        false,
        false,
    ),
    (
        "resume_task",
        "Resume task",
        "Resume a paused or failed coding task.",
        false,
        false,
        false,
    ),
    (
        "finish_task",
        "Finish task",
        "Finish a task with verification status and change summary.",
        false,
        false,
        false,
    ),
    (
        "task_context",
        "Task context",
        "Return a bounded durable task context for a new conversation.",
        true,
        false,
        false,
    ),
    (
        "list_task_events",
        "List task events",
        "Read task event history with pagination.",
        true,
        false,
        false,
    ),
    (
        "change_summary",
        "Change summary",
        "Explain what changed, why, and what evidence exists.",
        true,
        false,
        false,
    ),
    (
        "exec_health_check",
        "Exec health check",
        "Verify the exec worker, session creation, command execution, and stdout/stderr capture.",
        true,
        false,
        false,
    ),
    (
        "set_default_cwd",
        "Set default cwd",
        "Set the default cwd for relative tool paths inside the workspace.",
        true,
        false,
        false,
    ),
    (
        "read_file",
        "Read file",
        "Read a UTF-8 text file slice inside the configured workspace.",
        true,
        false,
        false,
    ),
    (
        "read_many",
        "Read many files",
        "Read multiple bounded UTF-8 file slices with hashes in one call.",
        true,
        false,
        false,
    ),
    (
        "project_map",
        "Project map",
        "Summarize manifests, languages, entrypoints, tests, commands, and a bounded project tree.",
        true,
        false,
        false,
    ),
    (
        "list_files",
        "List workspace entries",
        "List workspace files, directories, or symlinks using glob, depth, and entry-type filters.",
        true,
        false,
        false,
    ),
    (
        "search_text",
        "Search text",
        "Search UTF-8 workspace files for text or regex matches.",
        true,
        false,
        false,
    ),
    (
        "apply_patch",
        "Apply patch",
        "Apply a patch envelope transactionally inside the workspace.",
        false,
        true,
        false,
    ),
    (
        "edit_file",
        "Edit file precisely",
        "Apply guarded text or line edits to one file and return a diff. CRLF/LF differences are handled automatically while preserving the file's newline style.",
        false,
        true,
        false,
    ),
    (
        "edit_many",
        "Edit many files",
        "Apply guarded text edits to multiple files as one atomic transaction. CRLF/LF differences are handled automatically while preserving each file's newline style.",
        false,
        true,
        false,
    ),
    (
        "file_ops",
        "File operations",
        "Create, delete, copy, move, or create directories with transactional preflight.",
        false,
        true,
        false,
    ),
    (
        "format_files",
        "Format files",
        "Plan, check, or apply multi-language formatting in an isolated mirror with bounded diffs and guarded workspace writes.",
        false,
        true,
        false,
    ),
    (
        "patch_check",
        "Check patch",
        "Validate a patch without changing the workspace.",
        true,
        false,
        false,
    ),
    (
        "exec_command",
        "Execute command",
        "Run a bounded command in the workspace under runtime policy. When a session is retained, continue with wait_command using the returned next_actions arguments.",
        false,
        true,
        true,
    ),
    (
        "exec_many",
        "Execute command graph",
        "Run up to 256 structured exec_command requests sequentially, in parallel, or as a dependency DAG. Named lock groups serialize shared resources across batches.",
        false,
        true,
        true,
    ),
    (
        "wait_command",
        "Wait for command",
        "Wait for an exec_command session to produce new sequenced output, exit, or finish verification without client-side polling.",
        true,
        false,
        false,
    ),
    (
        "resolve_operation",
        "Resolve command operation",
        "Reattach to a retained command by operation_id or command_fingerprint without starting a duplicate process.",
        true,
        false,
        false,
    ),
    (
        "list_sessions",
        "List command sessions",
        "List bounded retained command sessions for recovery, diagnosis, and orphan cleanup.",
        true,
        false,
        false,
    ),
    (
        "send_input",
        "Send command input",
        "Write stdin or close stdin for a running command session without waiting for output.",
        false,
        false,
        false,
    ),
    (
        "kill_session",
        "Kill session",
        "Terminate a server-managed running command session.",
        false,
        true,
        false,
    ),
    (
        "read_output",
        "Read output",
        "Read retained stdout or stderr by output_ref with per-stream byte offset pagination.",
        true,
        false,
        false,
    ),
    (
        "git_status",
        "Git status",
        "Return git working tree status for the workspace.",
        true,
        false,
        false,
    ),
    (
        "git_diff",
        "Git diff",
        "Return unified git diff for workspace changes.",
        true,
        false,
        false,
    ),
    (
        "git_log",
        "Git log",
        "Return recent git commits with bounded structured metadata.",
        true,
        false,
        false,
    ),
    (
        "git_show",
        "Git show",
        "Return bounded git show output for a revision.",
        true,
        false,
        false,
    ),
    (
        "git_blame",
        "Git blame",
        "Return bounded git blame metadata for a workspace file.",
        true,
        false,
        false,
    ),
    (
        "git_branch",
        "Git branch",
        "Create, switch, or delete a Git branch with expected-HEAD guards.",
        false,
        true,
        false,
    ),
    (
        "git_stage",
        "Git stage",
        "Stage selected paths or all workspace changes with structured results.",
        false,
        true,
        false,
    ),
    (
        "git_commit",
        "Git commit",
        "Create a guarded Git commit and return the new commit and repository status.",
        false,
        true,
        false,
    ),
    (
        "git_restore",
        "Git restore",
        "Restore or unstage selected paths with explicit confirmation.",
        false,
        true,
        false,
    ),
    (
        "request_permissions",
        "Approve and resume operation",
        "Approve and immediately resume one pending permission-gated operation.",
        false,
        true,
        false,
    ),
    (
        "view_image",
        "View image",
        "Return a workspace image as MCP image content.",
        true,
        false,
        false,
    ),
];

/// old Python 版本默认提供的核心工具集。默认 MCP 只暴露这一组，保持 Agent 的工具面稳定。
pub const CORE_TOOLS: &[&str] = &[
    "server_info",
    "list_workspace_folders",
    "switch_workspace_folder",
    "query_tool_usage",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "set_default_cwd",
    "read_file",
    "read_many",
    "project_map",
    "list_files",
    "search_text",
    "apply_patch",
    "edit_file",
    "edit_many",
    "file_ops",
    "format_files",
    "exec_command",
    "exec_many",
    "wait_command",
    "resolve_operation",
    "list_sessions",
    "send_input",
    "kill_session",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_stage",
    "git_commit",
    "git_restore",
    "view_image",
];

pub const GUARDED_CORE_TOOLS: &[&str] = &[
    "server_info",
    "list_workspace_folders",
    "switch_workspace_folder",
    "query_tool_usage",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "set_default_cwd",
    "read_file",
    "read_many",
    "project_map",
    "list_files",
    "search_text",
    "apply_patch",
    "edit_file",
    "edit_many",
    "file_ops",
    "format_files",
    "exec_command",
    "exec_many",
    "wait_command",
    "resolve_operation",
    "list_sessions",
    "send_input",
    "kill_session",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_stage",
    "git_commit",
    "git_restore",
    "request_permissions",
    "view_image",
];

pub const CORE_READ_ONLY_TOOLS: &[&str] = &[
    "server_info",
    "list_workspace_folders",
    "query_tool_usage",
    "read_file",
    "read_many",
    "project_map",
    "list_files",
    "search_text",
    "wait_command",
    "resolve_operation",
    "list_sessions",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
];

pub const ALLOWED_TOOLS: &[&str] = &[
    "harness_status",
    "operation_log",
    "server_info",
    "list_workspace_folders",
    "switch_workspace_folder",
    "query_tool_usage",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "exec_health_check",
    "set_default_cwd",
    "read_file",
    "read_many",
    "project_map",
    "list_files",
    "search_text",
    "apply_patch",
    "edit_file",
    "edit_many",
    "file_ops",
    "format_files",
    "patch_check",
    "exec_command",
    "exec_many",
    "wait_command",
    "resolve_operation",
    "list_sessions",
    "send_input",
    "kill_session",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "git_branch",
    "git_stage",
    "git_commit",
    "git_restore",
    "project_state",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
    "task_context",
    "list_task_events",
    "change_summary",
    "request_permissions",
    "view_image",
];

pub const MUTATING_TOOLS: &[&str] = &[
    "switch_workspace_folder",
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "apply_patch",
    "edit_file",
    "edit_many",
    "file_ops",
    "format_files",
    "git_branch",
    "git_stage",
    "git_commit",
    "git_restore",
    "exec_command",
    "exec_many",
    "send_input",
    "kill_session",
    "request_permissions",
    "set_default_cwd",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
];

/// Actions requests that must be serialized because they directly update
/// workspace files, Git state, durable history, or task metadata. Process
/// lifecycle tools deliberately stay outside this lock so a long-running
/// command cannot block send_input/kill_session or unrelated control calls.
pub const SERIALIZED_WORKSPACE_TOOLS: &[&str] = &[
    "history_session_bootstrap",
    "history_session_checkpoint",
    "history_session_validate",
    "apply_patch",
    "edit_file",
    "edit_many",
    "file_ops",
    "format_files",
    "git_branch",
    "git_stage",
    "git_commit",
    "git_restore",
    "set_default_cwd",
    "start_task",
    "update_task",
    "pause_task",
    "resume_task",
    "finish_task",
];

pub const READ_ONLY_TOOLS: &[&str] = &[
    "list_workspace_folders",
    "harness_status",
    "operation_log",
    "server_info",
    "query_tool_usage",
    "exec_health_check",
    "read_file",
    "read_many",
    "project_map",
    "list_files",
    "search_text",
    "wait_command",
    "resolve_operation",
    "list_sessions",
    "read_output",
    "git_status",
    "git_diff",
    "git_log",
    "git_show",
    "git_blame",
    "view_image",
    "patch_check",
    "project_state",
    "task_context",
    "list_task_events",
    "change_summary",
];

pub fn is_allowed_tool(name: &str) -> bool {
    ALLOWED_TOOLS.contains(&name)
}

pub fn is_mcp_only_tool(name: &str) -> bool {
    name == "switch_workspace_folder"
}

pub fn canonical_tool_name(name: &str) -> &str {
    name
}

pub fn normalize_tool_profile(profile: &str) -> &'static str {
    match profile {
        "advanced" => "advanced",
        "read-only" => "read-only",
        "compat-readonly-all" => "compat-readonly-all",
        "guarded-core" => "guarded-core",
        "trusted-core" | "core" => "trusted-core",
        _ => "trusted-core",
    }
}

pub fn resolve_tool_profile(profile: &str, permission_mode: &str) -> &'static str {
    let normalized = normalize_tool_profile(profile);
    if normalized == "trusted-core"
        && permission_mode != "trusted"
        && permission_mode != "dangerous"
    {
        "guarded-core"
    } else {
        normalized
    }
}

pub fn exposed_tool_names(tool_profile: &str) -> Vec<&'static str> {
    match normalize_tool_profile(tool_profile) {
        "read-only" => CORE_READ_ONLY_TOOLS.to_vec(),
        "guarded-core" => GUARDED_CORE_TOOLS.to_vec(),
        "advanced" | "compat-readonly-all" => P0_TOOLS.iter().map(|(name, ..)| *name).collect(),
        _ => CORE_TOOLS.to_vec(),
    }
}

pub fn list_tools() -> Vec<Value> {
    list_tools_for_profile("full")
}

pub fn list_tools_for_profile(tool_profile: &str) -> Vec<Value> {
    let compat = tool_profile == "compat-readonly-all";
    exposed_tool_names(tool_profile)
        .into_iter()
        .filter_map(|name| {
            P0_TOOLS.iter().find(|(n, ..)| *n == name).map(|entry| {
                let (name, title, description, read_only, destructive, open_world) = *entry;
                let (read_only, destructive, open_world) = if compat {
                    (true, false, false)
                } else {
                    (read_only, destructive, open_world)
                };
                json!({
                    "name": name,
                    "title": title,
                    "description": description,
                    "inputSchema": input_schema(name),
                    "annotations": {
                        "title": title,
                        "readOnlyHint": read_only,
                        "destructiveHint": destructive,
                        "idempotentHint": read_only,
                        "openWorldHint": open_world
                    }
                })
            })
        })
        .collect()
}

pub fn toolset_revision(tool_profile: &str) -> String {
    let encoded = serde_json::to_vec(&list_tools_for_profile(tool_profile)).unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(encoded));
    digest[..16].to_string()
}

pub fn input_schema(name: &str) -> Value {
    match name {
        "list_workspace_folders" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "switch_workspace_folder" => json!({
            "type": "object",
            "required": ["folder_id"],
            "properties": {
                "folder_id": { "type": "string", "minLength": 1 }
            },
            "additionalProperties": false
        }),
        "query_tool_usage" => json!({
            "type": "object",
            "properties": {
                "tools": { "type": "array", "maxItems": 100, "items": { "type": "string" }, "default": [] },
                "exclude_tools": { "type": "array", "maxItems": 100, "items": { "type": "string" }, "default": ["query_tool_usage"] },
                "outcomes": { "type": "array", "maxItems": 20, "items": { "type": "string" }, "default": [] },
                "scope": { "type": "string", "enum": ["current_runtime", "current_version", "all"], "default": "current_runtime" },
                "sort_by": { "type": "string", "enum": ["calls", "errors", "duration_ms", "p95_ms", "response_bytes", "request_bytes", "queue_wait_ms"], "default": "calls" },
                "top": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                "errors_only": { "type": "boolean", "default": false },
                "min_duration_ms": { "type": "integer", "minimum": 0, "default": 0 },
                "since_ts_ms": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 },
                "aggregate": { "type": "boolean", "default": true },
                "include_records": { "type": "boolean", "default": false },
                "include_payloads": { "type": "boolean", "default": false },
                "include_slowest": { "type": "boolean", "default": true },
                "include_largest": { "type": "boolean", "default": true },
                "include_performance": { "type": "boolean", "default": true },
                "include_bursts": { "type": "boolean", "default": true },
                "include_async_sessions": { "type": "boolean", "default": true },
                "burst_idle_ms": { "type": "integer", "minimum": 1000, "maximum": 3600000, "default": 120000 }
            },
            "additionalProperties": false
        }),
        "history_session_bootstrap" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string" },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "create_if_missing": { "type": "boolean", "default": true }
            },
            "additionalProperties": false
        }),
        "history_session_checkpoint" => json!({
            "type": "object",
            "required": ["session_key", "expected_path"],
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "expected_path": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "turn_id": { "type": "string", "minLength": 1 },
                "timestamp": { "type": "string" },
                "user_intent": { "type": "string" },
                "findings": { "type": "array", "items": { "type": "string" } },
                "decisions": { "type": "array", "items": { "type": "string" } },
                "files_changed": { "type": "array", "items": { "type": "string" } },
                "tests": { "type": "array", "items": { "type": "string" } },
                "runtime_state": { "type": "array", "items": { "type": "string" } },
                "remaining_issues": { "type": "array", "items": { "type": "string" } },
                "next_actions": { "type": "array", "items": { "type": "string" } },
                "notes": { "type": "string" }
            },
            "additionalProperties": false
        }),
        "history_session_validate" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "repair": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "harness_status" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "exec_health_check" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        "operation_log" => json!({
            "type": "object",
            "properties": {
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "additionalProperties": false
        }),
        "project_state" => json!({
            "type": "object",
            "properties": {
                "max_files": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 200 }
            },
            "additionalProperties": false
        }),
        "start_task" => json!({
            "type": "object",
            "properties": {
                "objective": { "type": "string", "minLength": 1 }
            },
            "required": ["objective"],
            "additionalProperties": false
        }),
        "update_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "completed_steps": { "type": "array", "items": { "type": "string" } },
                "pending_steps": { "type": "array", "items": { "type": "string" } }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "pause_task" | "resume_task" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string", "minLength": 1 } },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "finish_task" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "summary": { "type": "string" },
                "allow_unverified": { "type": "boolean", "default": false }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "task_context" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string" },
                "max_bytes": { "type": "integer", "minimum": 8192, "maximum": 131072, "default": 32768 }
            },
            "additionalProperties": false
        }),
        "list_task_events" => json!({
            "type": "object",
            "properties": {
                "task_id": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 }
            },
            "required": ["task_id"],
            "additionalProperties": false
        }),
        "change_summary" => json!({
            "type": "object",
            "properties": { "task_id": { "type": "string" }, "change_id": { "type": "string" } },
            "additionalProperties": false
        }),
        "read_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 131072 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "read_many" => json!({
            "type": "object",
            "properties": {
                "items": {
                    "type": "array",
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1 },
                            "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                            "end_line": { "type": "integer", "minimum": 1 },
                            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576 }
                        },
                        "required": ["path"],
                        "additionalProperties": false
                    }
                },
                "matches": {
                    "type": "array",
                    "maxItems": 500,
                    "description": "Search match objects containing path and line, or explicit ranges.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1 },
                            "line": { "type": "integer", "minimum": 1 },
                            "start_line": { "type": "integer", "minimum": 1 },
                            "end_line": { "type": "integer", "minimum": 1 },
                            "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576 },
                            "match_id": { "type": "string" }
                        },
                        "required": ["path"],
                        "additionalProperties": true
                    }
                },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 500, "default": 20 },
                "merge_overlaps": { "type": "boolean", "default": true },
                "line_numbers": { "type": "boolean", "default": false },
                "max_total_bytes": { "type": "integer", "minimum": 1, "maximum": 4194304, "default": 262144 },
                "max_bytes_per_file": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 131072 }
            },
            "additionalProperties": false
        }),
        "project_map" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "max_files": { "type": "integer", "minimum": 1, "maximum": 50000, "default": 10000 },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 20, "default": 4 },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false }
            },
            "additionalProperties": false
        }),
        "list_files" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "patterns": { "type": "array", "items": { "type": "string" } },
                "glob": { "type": "string", "description": "Alias for a single patterns entry" },
                "exclude_patterns": { "type": "array", "items": { "type": "string" } },
                "entry_types": { "type": "array", "items": { "type": "string", "enum": ["file", "directory", "symlink"] }, "default": ["file", "symlink"] },
                "recursive": { "type": "boolean", "default": true },
                "max_depth": { "type": "integer", "minimum": 1, "maximum": 20, "default": 20 },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 50000, "default": 5000 }
            },
            "additionalProperties": false
        }),
        "search_text" => json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "minLength": 1 },
                "queries": {
                    "type": "array",
                    "maxItems": 50,
                    "items": {
                        "oneOf": [
                            { "type": "string", "minLength": 1 },
                            {
                                "type": "object",
                                "properties": {
                                    "query": { "type": "string", "minLength": 1 },
                                    "regex": { "type": "boolean" },
                                    "case_sensitive": { "type": "boolean" }
                                },
                                "required": ["query"],
                                "additionalProperties": false
                            }
                        ]
                    }
                },
                "filename_query": { "type": "string", "minLength": 1 },
                "filename_regex": { "type": "boolean", "default": false },
                "filename_case_sensitive": { "type": "boolean", "default": false },
                "path": { "type": "string", "default": "." },
                "glob": { "type": "string", "description": "Alias appended to include_globs" },
                "include_globs": { "type": "array", "items": { "type": "string" } },
                "exclude_globs": { "type": "array", "items": { "type": "string" } },
                "regex": { "type": "boolean", "default": false },
                "case_sensitive": { "type": "boolean", "default": false },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 1000, "default": 0, "description": "Values above 20 are normalized to 20" },
                "max_preview_bytes": { "type": "integer", "minimum": 1, "maximum": 65536, "default": 512, "description": "Normalized to the supported 64-4096 byte range" },
                "max_file_bytes": { "type": "integer", "minimum": 1024, "maximum": 134217728, "default": 8388608 },
                "max_matches_per_file": { "type": "integer", "minimum": 1, "maximum": 100000 },
                "files_only": { "type": "boolean", "default": false },
                "count_only": { "type": "boolean", "default": false },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "include_hidden": { "type": "boolean", "default": false },
                "include_ignored": { "type": "boolean", "default": false },
                "max_results": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 }
            },
            "additionalProperties": false
        }),
        "apply_patch" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 },
                "dry_run": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "expected_sha256": {
                    "type": "object",
                    "additionalProperties": { "type": "string", "minLength": 64, "maxLength": 64 }
                },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "edit_file" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "expected_sha256": { "type": "string", "minLength": 64, "maxLength": 64 },
                "edits": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["replace", "insert_before", "insert_after", "replace_lines", "delete_lines"] },
                            "old_text": { "type": "string" },
                            "new_text": { "type": "string" },
                            "anchor": { "type": "string" },
                            "text": { "type": "string" },
                            "expected_text": { "type": "string" },
                            "match_mode": { "type": "string", "enum": ["exact", "whitespace"], "default": "exact", "description": "exact requires identical content except CRLF/LF line endings; whitespace additionally tolerates whitespace differences. Inserted and replacement text is normalized to the target file's newline style." },
                            "before_context": { "type": "string" },
                            "after_context": { "type": "string" },
                            "expected_occurrences": { "type": "integer", "minimum": 1, "default": 1 },
                            "start_line": { "type": "integer", "minimum": 1 },
                            "end_line": { "type": "integer", "minimum": 1 }
                        },
                        "required": ["type"],
                        "additionalProperties": false
                    }
                },
                "apply_proposal": {
                    "type": "object",
                    "properties": {
                        "proposal_id": { "type": "string", "minLength": 1 },
                        "patch": {
                            "type": "string",
                            "maxLength": 65536,
                            "description": "Optional economical unified diff limited to one file and one hunk, applied only to the proposal replacement text."
                        },
                        "replacement": {
                            "type": "string",
                            "maxLength": 131072,
                            "description": "Optional complete final replacement for the proposal candidate region. Mutually exclusive with patch."
                        }
                    },
                    "required": ["proposal_id"],
                    "additionalProperties": false
                },
                "dry_run": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "edit_many" => json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1 },
                            "expected_sha256": { "type": "string", "minLength": 64, "maxLength": 64 },
                            "edits": {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": 100,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "type": { "type": "string", "enum": ["replace", "insert_before", "insert_after", "replace_lines", "delete_lines"] },
                                        "old_text": { "type": "string" },
                                        "new_text": { "type": "string" },
                                        "anchor": { "type": "string" },
                                        "text": { "type": "string" },
                                        "expected_text": { "type": "string" },
                                        "match_mode": { "type": "string", "enum": ["exact", "whitespace"], "default": "exact", "description": "exact requires identical content except CRLF/LF line endings; whitespace additionally tolerates whitespace differences. Inserted and replacement text is normalized to the target file's newline style." },
                                        "before_context": { "type": "string" },
                                        "after_context": { "type": "string" },
                                        "expected_occurrences": { "type": "integer", "minimum": 1, "default": 1 },
                                        "start_line": { "type": "integer", "minimum": 1 },
                                        "end_line": { "type": "integer", "minimum": 1 }
                                    },
                                    "required": ["type"],
                                    "additionalProperties": false
                                }
                            }
                        },
                        "required": ["path", "edits"],
                        "additionalProperties": false
                    }
                },
                "dry_run": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["files"],
            "additionalProperties": false
        }),
        "format_files" => json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "maxItems": 10000,
                    "items": { "type": "string", "minLength": 1 },
                    "description": "Explicit workspace-relative files or directories. Optional for changed, staged, and project scopes."
                },
                "scope": {
                    "type": "string",
                    "enum": ["files", "changed", "staged", "project"],
                    "default": "files"
                },
                "mode": {
                    "type": "string",
                    "enum": ["plan", "check", "apply"],
                    "default": "plan"
                },
                "formatter": {
                    "type": "string",
                    "default": "auto",
                    "description": "auto or a registered formatter adapter ID"
                },
                "strict": { "type": "boolean", "default": false },
                "include_patterns": {
                    "type": "array",
                    "maxItems": 100,
                    "items": { "type": "string", "minLength": 1 }
                },
                "exclude_patterns": {
                    "type": "array",
                    "maxItems": 100,
                    "items": { "type": "string", "minLength": 1 }
                },
                "max_files": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10000,
                    "default": 500
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600000,
                    "default": 120000
                },
                "expected_sha256": {
                    "type": "object",
                    "maxProperties": 10000,
                    "additionalProperties": {
                        "type": "string",
                        "minLength": 64,
                        "maxLength": 64
                    }
                },
                "confirm": { "type": "boolean", "default": false },
                "max_diff_bytes": {
                    "type": "integer",
                    "minimum": 1024,
                    "maximum": 1048576,
                    "default": 262144
                },
                "reason": { "type": "string", "default": "" }
            },
            "additionalProperties": false
        }),
        "file_ops" => json!({
            "type": "object",
            "properties": {
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["create", "delete", "copy", "move", "mkdir"] },
                            "path": { "type": "string", "minLength": 1 },
                            "destination": { "type": "string", "minLength": 1 },
                            "content": { "type": "string" },
                            "expected_sha256": { "type": "string", "minLength": 64, "maxLength": 64 },
                            "overwrite": { "type": "boolean", "default": false }
                        },
                        "required": ["type", "path"],
                        "additionalProperties": false
                    }
                },
                "dry_run": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["operations"],
            "additionalProperties": false
        }),
        "patch_check" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "exec_many" => json!({
            "type": "object",
            "properties": {
                "commands": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 256,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Stable command identifier used by DAG dependencies" },
                            "depends_on": { "type": "array", "maxItems": 256, "items": { "type": "string", "minLength": 1, "maxLength": 128 }, "default": [], "description": "Command IDs that must succeed before this command can run in dag mode" },
                            "lock_group": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Shared named resource lock such as cargo-target, node-generated, or git-index" },
                            "operation_id": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Stable idempotency key. Retries with the same command reattach to the retained session." },
                            "deduplicate": { "type": "boolean", "description": "Coalesce identical retries. Defaults to true for safe Cargo check, test, build, and format commands." },
                            "cmd": { "type": "string", "minLength": 1 },
                            "script": { "type": "string", "minLength": 1, "description": "Structured shell script body; requires shell other than none" },
                            "program": { "type": "string", "minLength": 1 },
                            "args": { "type": "array", "maxItems": 1000, "items": { "type": "string" }, "default": [] },
                            "shell": { "type": "string", "enum": ["none", "cmd", "powershell", "sh"], "default": "none" },
                            "env": { "type": "object", "maxProperties": 64, "additionalProperties": { "type": "string", "maxLength": 4096 } },
                            "remove_env": { "type": "array", "maxItems": 64, "items": { "type": "string" } },
                            "workdir": { "type": "string", "default": "." },
                            "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000, "default": 30000 },
                            "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 65536 },
                            "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 30000 },
                            "output_mode": { "type": "string", "enum": ["tail", "none", "summary"], "default": "tail" },
                            "tty": { "type": "boolean", "default": false },
                            "stdin": { "type": "string", "default": "" },
                            "confirm": { "type": "boolean", "default": false },
                            "filesystem_scope": { "type": "string", "enum": ["workspace"], "default": "workspace" },
                            "reason": { "type": "string", "default": "" }
                        },
                        "additionalProperties": false
                    }
                },
                "mode": { "type": "string", "enum": ["sequential", "parallel", "dag"], "default": "sequential", "description": "Execution scheduler; sequential preserves legacy behavior" },
                "max_parallel": { "type": "integer", "minimum": 1, "maximum": 256, "description": "Maximum concurrently running batch commands" },
                "stop_on_error": { "type": "boolean", "default": true }
            },
            "required": ["commands"],
            "additionalProperties": false
        }),
        "exec_command" => {
            let mut schema = json!({
                "type": "object",
                "properties": {
                    "cmd": { "type": "string", "minLength": 1, "description": "Legacy command string or explicit shell command" },
                    "script": { "type": "string", "minLength": 1, "description": "Structured shell script body; requires shell other than none" },
                    "program": { "type": "string", "minLength": 1, "description": "Executable name or workspace-local path for shell-free execution" },
                    "args": { "type": "array", "maxItems": 1000, "items": { "type": "string" }, "default": [] },
                    "shell": { "type": "string", "enum": ["none", "cmd", "powershell", "sh"], "default": "none" },
                    "env": { "type": "object", "maxProperties": 64, "additionalProperties": { "type": "string", "maxLength": 4096 } },
                    "remove_env": { "type": "array", "maxItems": 64, "items": { "type": "string" } },
                    "workdir": { "type": "string", "default": "." },
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000, "default": 30000 },
                    "max_output_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 65536 },
                    "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 1000 },
                    "output_mode": { "type": "string", "enum": ["delta", "tail", "all", "none", "summary"], "default": "tail" },
                    "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                    "tail_lines": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 100 },
                    "tty": { "type": "boolean", "default": false },
                    "stdin": { "type": "string", "default": "" },
                    "post_checks": {
                        "type": "array",
                        "maxItems": 16,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": { "type": "string", "maxLength": 256 },
                                "cmd": { "type": "string", "minLength": 1 },
                                "script": { "type": "string", "minLength": 1 },
                                "program": { "type": "string", "minLength": 1 },
                                "args": { "type": "array", "maxItems": 1000, "items": { "type": "string" }, "default": [] },
                                "shell": { "type": "string", "enum": ["none", "cmd", "powershell", "sh"], "default": "none" },
                                "env": { "type": "object", "maxProperties": 64, "additionalProperties": { "type": "string", "maxLength": 4096 } },
                                "remove_env": { "type": "array", "maxItems": 64, "items": { "type": "string" } },
                                "expected_exit_code": { "type": "integer", "default": 0 },
                                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 600000, "default": 30000 },
                                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 16384 },
                                "confirm": { "type": "boolean", "default": false }
                            },
                            "additionalProperties": false
                        }
                    },
                    "confirm": { "type": "boolean", "default": false },
                    "filesystem_scope": { "type": "string", "enum": ["workspace"], "default": "workspace" },
                    "reason": { "type": "string", "default": "" }
                },
                "additionalProperties": false
            });
            let properties = schema
                .get_mut("properties")
                .and_then(Value::as_object_mut)
                .expect("exec_command schema properties");
            properties.insert(
                "operation_id".into(),
                json!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 128,
                    "description": "Stable idempotency key used to reattach retries instead of spawning a duplicate process."
                }),
            );
            properties.insert(
                "deduplicate".into(),
                json!({
                    "type": "boolean",
                    "description": "Coalesce identical retries. Defaults to true for safe Cargo check, test, build, and format commands."
                }),
            );
            properties.insert(
                "lock_group".into(),
                json!({
                    "type": "string",
                    "minLength": 1,
                    "maxLength": 128,
                    "description": "Shared resource lock. Cargo commands automatically derive a lock from their target directory."
                }),
            );
            schema
        }
        "wait_command" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "timeout_ms": { "type": "integer", "minimum": 0, "maximum": 120000, "default": 30000, "description": "Server-side event wait. Use until=finalized with a longer timeout for quiet long-running commands to reduce repeated polling." },
                "heartbeat_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 0, "description": "Return a heartbeat at this interval while the process remains quiet so transports stay active without restarting the command." },
                "until": { "type": "string", "enum": ["output_or_exit", "exit", "finalized"], "default": "output_or_exit" },
                "output_mode": { "type": "string", "enum": ["delta", "tail", "all", "none", "summary"], "default": "delta" },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 },
                "tail_lines": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 100 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "resolve_operation" => json!({
            "type": "object",
            "properties": {
                "operation_id": { "type": "string", "minLength": 1, "maxLength": 128 },
                "command_fingerprint": { "type": "string", "minLength": 64, "maxLength": 64 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "output_mode": { "type": "string", "enum": ["delta", "tail", "all", "none", "summary"], "default": "tail" },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 },
                "tail_lines": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 100 }
            },
            "additionalProperties": false
        }),
        "list_sessions" => json!({
            "type": "object",
            "properties": {
                "include_finalized": { "type": "boolean", "default": true },
                "status": { "type": "string", "enum": ["running", "verifying", "exited", "timed_out", "killed"] },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
            },
            "additionalProperties": false
        }),
        "send_input" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "chars": { "type": "string", "default": "" },
                "close_stdin": { "type": "boolean", "default": false }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "kill_session" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "signal": { "type": "string", "enum": ["TERM", "KILL", "INT"], "default": "TERM" },
                "wait_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 5000 },
                "max_output_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 65536 }
            },
            "required": ["session_id"],
            "additionalProperties": false
        }),
        "read_output" => json!({
            "type": "object",
            "properties": {
                "output_ref": { "type": "string", "minLength": 1 },
                "offset": { "type": "integer", "minimum": 0, "default": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 4096 }
            },
            "required": ["output_ref"],
            "additionalProperties": false
        }),
        "git_status" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "include_untracked": { "type": "boolean", "default": true },
                "max_entries": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 1000 }
            },
            "additionalProperties": false
        }),
        "git_diff" => json!({
            "type": "object",
            "properties": {
                "paths": { "type": "array", "items": { "type": "string" }, "default": [] },
                "staged": { "type": "boolean", "default": false },
                "unstaged": { "type": "boolean", "default": true },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 1000, "default": 3, "description": "Values above 20 are normalized to 20" },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 1048576, "default": 262144 }
            },
            "additionalProperties": false
        }),
        "git_log" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." },
                "ref": { "type": "string", "default": "HEAD" },
                "max_count": { "type": "integer", "minimum": 1, "maximum": 100, "default": 20 },
                "skip": { "type": "integer", "minimum": 0, "maximum": 10000, "default": 0 }
            },
            "additionalProperties": false
        }),
        "git_show" => json!({
            "type": "object",
            "properties": {
                "rev": { "type": "string", "default": "HEAD" },
                "path": { "type": "string" },
                "paths": { "type": "array", "items": { "type": "string" } },
                "include_diff": { "type": "boolean", "default": true },
                "context_lines": { "type": "integer", "minimum": 0, "maximum": 1000, "default": 3, "description": "Values above 20 are normalized to 20" },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": 1048576, "default": 262144 }
            },
            "additionalProperties": false
        }),
        "git_blame" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "rev": { "type": "string" },
                "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                "end_line": { "type": "integer", "minimum": 1 },
                "max_lines": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 200 }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        "git_branch" => json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["create", "switch", "delete"] },
                "name": { "type": "string", "minLength": 1 },
                "start_point": { "type": "string" },
                "switch": { "type": "boolean", "default": true },
                "force": { "type": "boolean", "default": false },
                "expected_head": { "type": "string" },
                "dry_run": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["action", "name"],
            "additionalProperties": false
        }),
        "git_stage" => json!({
            "type": "object",
            "properties": {
                "paths": { "type": "array", "items": { "type": "string" } },
                "all": { "type": "boolean", "default": false },
                "expected_head": { "type": "string" },
                "dry_run": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "additionalProperties": false
        }),
        "git_commit" => json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "minLength": 1, "maxLength": 10000 },
                "paths": { "type": "array", "items": { "type": "string" } },
                "all": { "type": "boolean", "default": false },
                "allow_empty": { "type": "boolean", "default": false },
                "require_clean_index_before": { "type": "boolean" },
                "expected_head": { "type": "string" },
                "dry_run": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["message"],
            "additionalProperties": false
        }),
        "git_restore" => json!({
            "type": "object",
            "properties": {
                "paths": { "type": "array", "minItems": 1, "items": { "type": "string" } },
                "staged": { "type": "boolean", "default": false },
                "worktree": { "type": "boolean" },
                "source": { "type": "string" },
                "expected_head": { "type": "string" },
                "dry_run": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["paths"],
            "additionalProperties": false
        }),
        "request_permissions" => json!({
            "type": "object",
            "properties": {
                "resume_id": { "type": "string", "minLength": 1 },
                "approve": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "tool_name": {
                    "type": "string",
                    "enum": ["exec_command", "apply_patch"]
                },
                "permission": {
                    "type": "string",
                    "enum": [
                        "network",
                        "destructive_command",
                        "long_timeout",
                        "sensitive_env",
                        "shell_expansion",
                        "inline_script",
                        "privileged_executable",
                        "write_generated_or_ignored"
                    ]
                },
                "reason": { "type": "string", "minLength": 1 },
                "arguments": { "type": "object", "additionalProperties": true },
                "scope": {
                    "type": "string",
                    "enum": ["once", "session"],
                    "default": "once"
                },
                "ttl_seconds": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 3600,
                    "default": 300
                }
            },
            "additionalProperties": false
        }),
        "set_default_cwd" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "default": "." }
            },
            "additionalProperties": false
        }),
        "view_image" => json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 10485760, "default": 5242880 },
                "max_width": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "max_height": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "auto_resize": { "type": "boolean", "default": true },
                "output": { "type": "string", "enum": ["mcp_image", "data_url"], "default": "mcp_image" }
            },
            "required": ["path"],
            "additionalProperties": false
        }),
        _ => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{input_schema, list_tools_for_profile};

    #[test]
    fn trusted_core_catalog_exposes_33_non_overlapping_tools() {
        let tools = list_tools_for_profile("core");
        let names: Vec<_> = tools
            .iter()
            .map(|tool| tool["name"].as_str().expect("tool name"))
            .collect();
        let unique: HashSet<_> = names.iter().copied().collect();

        assert_eq!(tools.len(), 35);
        assert_eq!(unique.len(), tools.len());
        assert!(names.contains(&"history_session_bootstrap"));
        assert!(names.contains(&"history_session_checkpoint"));
        assert!(names.contains(&"list_workspace_folders"));
        assert!(names.contains(&"switch_workspace_folder"));
        assert!(names.contains(&"read_many"));
        assert!(names.contains(&"edit_file"));
        assert!(names.contains(&"format_files"));
        assert!(names.contains(&"wait_command"));
        assert!(names.contains(&"resolve_operation"));
        assert!(names.contains(&"list_sessions"));
        assert!(names.contains(&"send_input"));
        for removed in [
            "history_session_validate",
            "check_exec_environment",
            "get_default_cwd",
            "list_dir",
            "grep_text",
            "request_permissions",
        ] {
            assert!(!names.contains(&removed), "{removed} should not be exposed");
        }

        for name in names {
            let schema = input_schema(name);
            assert_eq!(schema["type"], "object", "{name} schema type");
            assert!(schema["properties"].is_object(), "{name} properties");
            assert!(schema.get("oneOf").is_none(), "{name} oneOf");
            assert!(schema.get("anyOf").is_none(), "{name} anyOf");
            assert!(schema.get("$ref").is_none(), "{name} ref");
        }
    }

    #[test]
    fn guarded_core_adds_only_permission_requests() {
        let trusted = list_tools_for_profile("trusted-core");
        let guarded = list_tools_for_profile("guarded-core");
        assert_eq!(trusted.len(), 35);
        assert_eq!(guarded.len(), 36);
        assert!(guarded
            .iter()
            .any(|tool| tool["name"] == "request_permissions"));
    }

    #[test]
    fn public_schemas_allow_values_that_the_server_can_normalize() {
        let search_schema = input_schema("search_text");
        assert_eq!(
            search_schema["properties"]["context_lines"]["maximum"],
            1000
        );
        assert_eq!(
            search_schema["properties"]["max_preview_bytes"]["maximum"],
            65_536
        );

        let diff_schema = input_schema("git_diff");
        assert_eq!(diff_schema["properties"]["context_lines"]["maximum"], 1000);

        let wait_schema = input_schema("wait_command");
        assert_eq!(wait_schema["properties"]["timeout_ms"]["maximum"], 120_000);
        assert_eq!(wait_schema["properties"]["heartbeat_ms"]["maximum"], 30_000);

        let exec_schema = input_schema("exec_command");
        assert!(exec_schema["properties"]["operation_id"].is_object());
        assert!(exec_schema["properties"]["deduplicate"].is_object());
        assert!(exec_schema["properties"]["lock_group"].is_object());

        let resolve_schema = input_schema("resolve_operation");
        assert!(resolve_schema["properties"]["operation_id"].is_object());
        assert!(resolve_schema["properties"]["command_fingerprint"].is_object());

        let format_schema = input_schema("format_files");
        assert_eq!(format_schema["properties"]["mode"]["default"], "plan");
        assert_eq!(format_schema["properties"]["scope"]["default"], "files");
        assert_eq!(format_schema["properties"]["max_files"]["maximum"], 10_000);
        assert_eq!(
            format_schema["properties"]["timeout_ms"]["maximum"],
            600_000
        );
        assert!(format_schema["properties"]["expected_sha256"]["additionalProperties"].is_object());
    }
}
