use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::tools::{
    call_tool, call_tool_async, list_tools_for_profile, wrap_mcp_tool_result, ExecutionLimits,
    SharedToolContext, ToolContext, Workspace,
};
use crate::workspace::{AuthConfig, WorkspaceFolder};

pub type SharedState = SharedToolContext;

pub const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &[LATEST_PROTOCOL_VERSION, "2025-06-18", "2025-03-26"];

pub fn is_supported_protocol_version(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

pub fn handle_request(state: &SharedState, body: &Value) -> Value {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    if id.is_null() && method.starts_with("notifications/") {
        return Value::Null;
    }

    let result = match method {
        "initialize" => Ok(initialize_result(
            params.get("protocolVersion").and_then(Value::as_str),
            &state.tool_profile,
        )),
        "ping" => Ok(serde_json::json!({})),
        "tools/list" => {
            let tools = list_tools_for_profile(&state.tool_profile);
            Ok(serde_json::json!({
                "tools": tools,
                "toolsetRevision": crate::tools::registry::toolset_revision(&state.tool_profile)
            }))
        }
        "tools/call" => handle_tools_call(state, &params),
        _ => Err(serde_json::json!({
            "code": -32601,
            "message": format!("Method not found: {method}")
        })),
    };

    match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

pub async fn handle_request_async(state: SharedState, body: Value) -> Value {
    let method = body.get("method").and_then(Value::as_str).unwrap_or("");
    if method != "tools/call" {
        return handle_request(&state, &body);
    }

    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let params = body.get("params").cloned().unwrap_or(Value::Null);
    let result = handle_tools_call_async(state, params).await;
    match result {
        Ok(result) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "error": error }),
    }
}

fn initialize_result(requested_version: Option<&str>, tool_profile: &str) -> Value {
    let protocol_version = requested_version
        .filter(|version| is_supported_protocol_version(version))
        .unwrap_or(LATEST_PROTOCOL_VERSION);
    serde_json::json!({
        "protocolVersion": protocol_version,
        "capabilities": {
            "tools": { "listChanged": false },
            "logging": {}
        },
        "serverInfo": {
            "name": "coding-tools-mcp",
            "title": "Coding Tools MCP",
            "version": env!("CARGO_PKG_VERSION"),
            "toolsetRevision": crate::tools::registry::toolset_revision(tool_profile)
        },
        "instructions": "Use these tools only for local coding operations inside the configured tool hub. A tool hub may contain multiple allowed folders while sharing one MCP endpoint. At the start of every new ChatGPT conversation, before accessing project content, call list_workspace_folders and then call switch_workspace_folder to explicitly bind this conversation to one allowed folder. There is no default folder and no history-based folder fallback. Until this conversation is bound, all project tools fail with WORKSPACE_FOLDER_NOT_SELECTED. The selected folder and default cwd are remembered for the same runtime session without affecting other conversations. After binding, call history_session_bootstrap exactly once, even if the user did not explicitly ask to restore or resume. Treat bootstrap as required conversation initialization: when no history exists it creates the first history session; when history exists, read all_history_summary, latest_handoff, and inherited_summary before acting. Repeated successful bootstrap calls in the same conversation resume the same session and must not create duplicates. Preserve session_key and current_path returned by bootstrap, then pass them unchanged as session_key and expected_path to every history_session_checkpoint call. After completing each user-requested task in the conversation, call history_session_checkpoint before the final response. Only state that progress was saved after checkpoint returns ok=true with the same session_key and path. Persistence requires a successful tool call and is not automatic background persistence."
    })
}

fn handle_tools_call(state: &SharedState, params: &Value) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing tool name" }))?;
    let args = tool_arguments(name, params);

    let canonical_name = crate::tools::registry::canonical_tool_name(name);
    let known = crate::tools::registry::exposed_tool_names(&state.tool_profile);
    if !known.iter().any(|n| n == &canonical_name) {
        return Err(unknown_tool_error(state, name, &known));
    }

    let host_session_key = host_session_key(params);
    let structured = match canonical_name {
        "list_workspace_folders" => {
            crate::tools::hub::list_workspace_folders(state, host_session_key)
        }
        "switch_workspace_folder" => {
            let folder_id = args
                .get("folder_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            crate::tools::hub::switch_workspace_folder(state, folder_id, host_session_key)
        }
        _ => {
            let selected = crate::tools::hub::resolve_context(state.clone(), host_session_key)
                .map_err(|message| workspace_routing_error(state, host_session_key, message))?;
            call_tool(selected.as_ref(), canonical_name, &args)
        }
    };
    Ok(wrap_mcp_tool_result(canonical_name, &args, structured))
}

async fn handle_tools_call_async(state: SharedState, params: Value) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| serde_json::json!({ "code": -32602, "message": "Missing tool name" }))?;
    let args = tool_arguments(name, &params);
    let canonical_name = crate::tools::registry::canonical_tool_name(name).to_string();
    let known = crate::tools::registry::exposed_tool_names(&state.tool_profile);
    if !known.iter().any(|known_name| known_name == &canonical_name) {
        return Err(unknown_tool_error(&state, name, &known));
    }

    let host_session_key = host_session_key(&params);
    let structured = match canonical_name.as_str() {
        "list_workspace_folders" => {
            crate::tools::hub::list_workspace_folders(&state, host_session_key)
        }
        "switch_workspace_folder" => {
            let folder_id = args
                .get("folder_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            crate::tools::hub::switch_workspace_folder(&state, folder_id, host_session_key)
        }
        _ => {
            let selected = crate::tools::hub::resolve_context(state.clone(), host_session_key)
                .map_err(|message| workspace_routing_error(&state, host_session_key, message))?;
            call_tool_async(selected, canonical_name.clone(), args.clone()).await
        }
    };
    Ok(wrap_mcp_tool_result(&canonical_name, &args, structured))
}

fn workspace_routing_error(
    state: &SharedState,
    host_session_key: Option<&str>,
    message: String,
) -> Value {
    let (error_code, message) =
        crate::tools::hub::routing_error_parts(&message, "WORKSPACE_FOLDER_ROUTING_FAILED");
    let mut data = serde_json::json!({
        "reason": "workspace_folder_routing_failed",
        "error_code": error_code,
        "error_category": "workspace_routing",
        "retryable": true,
        "suggestion": "Call list_workspace_folders and then switch_workspace_folder with an allowed folder_id."
    });
    if error_code == "WORKSPACE_FOLDER_NOT_SELECTED" {
        let listing = crate::tools::hub::list_workspace_folders(state, host_session_key);
        data["available_folders"] = crate::tools::hub::routing_folder_options(&listing);
        data["selected_folder_id"] = listing
            .get("selected_folder_id")
            .cloned()
            .unwrap_or(Value::Null);
        data["next_action"] = Value::String(
            "Choose one available_folders entry and call switch_workspace_folder with its id."
                .into(),
        );
        data["suggestion"] = Value::String(
            "Choose an available folder and call switch_workspace_folder; no default folder is selected."
                .into(),
        );
    }
    serde_json::json!({
        "code": -32602,
        "message": message,
        "data": data
    })
}

fn host_session_key(params: &Value) -> Option<&str> {
    params
        .get("_meta")
        .and_then(|meta| meta.get("openai/session"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn unknown_tool_error(state: &SharedState, name: &str, known: &[&str]) -> Value {
    serde_json::json!({
        "code": -32602,
        "message": format!("Unknown tool: {name}"),
        "data": {
            "reason": "unknown_tool",
            "error_code": "UNKNOWN_TOOL",
            "error_category": "catalog",
            "retryable": true,
            "suggestion": "Refresh tools/list and retry with the current tool catalog.",
            "toolset_revision": crate::tools::registry::toolset_revision(&state.tool_profile),
            "available_tools": known
        }
    })
}

fn legacy_edit_file_arguments(args: Value) -> Value {
    let Some(source) = args.as_object() else {
        return args;
    };
    let mut file = serde_json::Map::new();
    for field in ["path", "expected_sha256", "edits", "apply_proposal"] {
        if let Some(value) = source.get(field) {
            file.insert(field.to_string(), value.clone());
        }
    }
    let mut converted = serde_json::Map::new();
    converted.insert("files".into(), Value::Array(vec![Value::Object(file)]));
    for field in ["dry_run", "reason"] {
        if let Some(value) = source.get(field) {
            converted.insert(field.to_string(), value.clone());
        }
    }
    Value::Object(converted)
}

fn tool_arguments(name: &str, params: &Value) -> Value {
    let mut args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    if name == "edit_file" {
        args = legacy_edit_file_arguments(args);
    }
    if name.starts_with("history_session_") {
        if let Some(session_key) = host_session_key(params) {
            if !args.is_object() {
                args = serde_json::json!({});
            }
            args["_host_session_key"] = Value::String(session_key.to_string());
        }
    }
    args
}

pub fn new_state(
    folders: Vec<WorkspaceFolder>,
    bootstrap_folder_id: String,
    profile_id: String,
    auth: AuthConfig,
    policy: crate::tools::policy::PolicySettings,
    tool_profile: String,
    permission_mode: String,
    limits: ExecutionLimits,
) -> Result<SharedState, String> {
    let bootstrap_folder = folders
        .iter()
        .find(|folder| folder.id == bootstrap_folder_id)
        .cloned()
        .ok_or_else(|| "找不到 MCP 啟動用資料夾；不會 fallback 到第一個資料夾。".to_string())?;
    let workspace = Workspace::new_with_execution(
        PathBuf::from(&bootstrap_folder.path),
        bootstrap_folder.execution.clone(),
    )
    .map_err(|error| error.message())?;
    let state = Arc::new(
        ToolContext::from_workspace_with_profile_id_and_resource_id_and_limits(
            workspace,
            auth.clone(),
            policy.clone(),
            tool_profile.clone(),
            permission_mode.clone(),
            profile_id.clone(),
            format!("{}--mcp--{}", profile_id, bootstrap_folder.id),
            limits,
        ),
    );
    crate::tools::hub::register(
        profile_id,
        folders,
        bootstrap_folder.id,
        state.clone(),
        crate::tools::hub::HubConfig {
            auth,
            policy,
            tool_profile,
            permission_mode,
            limits,
            execution_resource_namespace: "mcp".into(),
        },
    )?;
    Ok(state)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use serde_json::json;

    use crate::tools::ToolContext;

    use super::{
        handle_request, handle_request_async, initialize_result, new_state, tool_arguments,
        LATEST_PROTOCOL_VERSION,
    };

    #[test]
    fn initialize_instructions_define_the_history_persistence_workflow() {
        let initialized = initialize_result(Some("2025-06-18"), "core");
        let instructions = initialized["instructions"].as_str().expect("instructions");
        assert!(instructions.contains("history_session_bootstrap"));
        assert!(instructions.contains("list_workspace_folders"));
        assert!(instructions.contains("switch_workspace_folder"));
        assert!(instructions.contains("without affecting other conversations"));
        assert!(instructions.contains("At the start of every new ChatGPT conversation"));
        assert!(instructions.contains("before accessing project content"));
        assert!(instructions.contains("There is no default folder"));
        assert!(instructions.contains("no history-based folder fallback"));
        assert!(instructions.contains("WORKSPACE_FOLDER_NOT_SELECTED"));
        assert!(instructions.contains("default cwd are remembered for the same runtime session"));
        assert!(instructions.contains("even if the user did not explicitly ask"));
        assert!(instructions.contains("required conversation initialization"));
        assert!(instructions.contains("must not create duplicates"));
        assert!(instructions.contains("history_session_checkpoint"));
        assert!(instructions.contains("session_key and current_path returned by bootstrap"));
        assert!(instructions.contains("session_key and expected_path"));
        assert!(instructions.contains("After completing each user-requested task"));
        assert!(instructions.contains("before the final response"));
        assert!(instructions.contains("checkpoint returns ok=true"));
        assert!(instructions.contains("not automatic background persistence"));
    }

    #[test]
    fn mcp_session_cannot_access_project_before_explicit_folder_selection() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        fs::write(workspace.path().join("README.md"), "explicit routing").expect("write readme");
        let profile_id = format!("explicit-routing-{}", uuid::Uuid::new_v4());
        let state = new_state(
            vec![crate::workspace::WorkspaceFolder {
                id: "folder-a".into(),
                name: "Folder A".into(),
                path: workspace.path().display().to_string(),
                execution: Default::default(),
            }],
            "folder-a".into(),
            profile_id.clone(),
            crate::workspace::AuthConfig::default(),
            crate::tools::policy::PolicySettings::default(),
            "full".into(),
            "trusted".into(),
            crate::tools::ExecutionLimits::default(),
        )
        .expect("mcp state");

        let unselected = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "read_file",
                    "arguments": {"path": "README.md"},
                    "_meta": {"openai/session": "session-a"}
                }
            }),
        );
        assert_eq!(
            unselected["error"]["data"]["error_code"],
            "WORKSPACE_FOLDER_NOT_SELECTED"
        );
        assert_eq!(
            unselected["error"]["data"]["available_folders"],
            json!([{
                "id": "folder-a",
                "name": "Folder A",
                "path": workspace.path().display().to_string()
            }])
        );
        assert!(unselected["error"]["data"]["selected_folder_id"].is_null());
        assert!(unselected["error"]["data"]["next_action"]
            .as_str()
            .expect("next action")
            .contains("switch_workspace_folder"));

        let listing = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "list_workspace_folders",
                    "arguments": {},
                    "_meta": {"openai/session": "session-a"}
                }
            }),
        );
        let listed = &listing["result"]["structuredContent"];
        assert!(listed.get("default_folder_id").is_none());
        assert!(listed["selected_folder_id"].is_null());

        let switched = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "switch_workspace_folder",
                    "arguments": {"folder_id": "folder-a"},
                    "_meta": {"openai/session": "session-a"}
                }
            }),
        );
        assert_eq!(switched["result"]["structuredContent"]["ok"], true);

        let selected = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {
                    "name": "read_file",
                    "arguments": {"path": "README.md"},
                    "_meta": {"openai/session": "session-a"}
                }
            }),
        );
        assert_eq!(selected["result"]["structuredContent"]["ok"], true);

        crate::tools::hub::remove_live_hub(&profile_id);
    }

    #[test]
    fn initialize_does_not_claim_tool_catalog_notifications_without_a_stream() {
        let initialized = initialize_result(Some("2025-06-18"), "core");

        assert_eq!(initialized["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(
            initialized["serverInfo"]["toolsetRevision"]
                .as_str()
                .expect("toolset revision")
                .len(),
            16
        );
    }

    #[test]
    fn initialize_negotiates_supported_versions_and_falls_back_to_latest() {
        assert_eq!(
            initialize_result(Some("2025-06-18"), "core")["protocolVersion"],
            "2025-06-18"
        );
        assert_eq!(
            initialize_result(Some("unsupported"), "core")["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );
    }

    #[test]
    fn workspace_prompt_initializes_or_restores_a_chatgpt_session() {
        let component = include_str!("../../../src/lib/components/ChatGptSessionPrompt.svelte");
        let catalog = include_str!("../../../src/lib/i18n/catalog.ts");

        assert!(component.contains("ChatGPT new-session prompt"));
        assert!(component.contains("Session bootstrap prompt"));
        assert!(catalog.contains("请初始化或恢复项目会话"));
        assert!(catalog.contains("必须使用目标 folder_id 调用 switch_workspace_folder"));
        assert!(catalog.contains("系统没有默认文件夹"));
        assert!(!catalog.contains("如果目标不是当前文件夹"));
        assert!(catalog.contains("如果没有历史记录"));
        assert!(catalog.contains("all_history_summary"));
        assert!(catalog.contains("history_session_checkpoint"));
        assert!(!component.contains("打开连接器设置"));
    }

    #[test]
    fn chatgpt_session_metadata_is_injected_only_for_history_tools() {
        let params = json!({
            "arguments": {"session_key": "explicit"},
            "_meta": {"openai/session": "chatgpt-conversation"}
        });
        let history = tool_arguments("history_session_bootstrap", &params);
        assert_eq!(history["session_key"], "explicit");
        assert_eq!(history["_host_session_key"], "chatgpt-conversation");

        let existing = tool_arguments("read_file", &params);
        assert_eq!(existing["session_key"], "explicit");
        assert!(existing.get("_host_session_key").is_none());
    }

    #[test]
    fn legacy_edit_names_are_canonicalized_before_catalog_validation() {
        let params = json!({
            "arguments": {
                "path": "main.rs",
                "expected_sha256": "a".repeat(64),
                "edits": [{
                    "type": "replace",
                    "old_text": "old",
                    "new_text": "new"
                }],
                "dry_run": true,
                "reason": "legacy compatibility"
            }
        });
        let arguments = tool_arguments("edit_file", &params);
        assert_eq!(
            crate::tools::registry::canonical_tool_name("edit_file"),
            "edit"
        );
        assert_eq!(
            crate::tools::registry::canonical_tool_name("edit_many"),
            "edit"
        );
        assert_eq!(arguments["files"].as_array().unwrap().len(), 1);
        assert_eq!(arguments["files"][0]["path"], "main.rs");
        assert_eq!(arguments["files"][0]["expected_sha256"], "a".repeat(64));
        assert_eq!(arguments["files"][0]["edits"].as_array().unwrap().len(), 1);
        assert_eq!(arguments["dry_run"], true);
        assert_eq!(arguments["reason"], "legacy compatibility");
        assert!(arguments.get("path").is_none());
    }

    #[test]
    fn explicit_session_key_prevents_changed_chatgpt_metadata_from_redirecting_history() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "history_session_bootstrap",
                    "arguments": {"session_key": "explicit-session"},
                    "_meta": {"openai/session": "chatgpt-session"}
                }
            }),
        );
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["ok"], true);
        assert_eq!(structured["session_key_source"], "explicit_session_key");
        assert_eq!(structured["session_key"], "explicit-session");
        assert_eq!(structured["host_session_key_mismatch"], true);
        let content = fs::read_to_string(workspace.path().join("docs/history-session/1.md"))
            .expect("read history file");
        assert!(content.contains("**Session key:** explicit-session"));
        assert!(!content.contains("**Session key:** chatgpt-session"));
    }

    #[test]
    fn removed_tool_names_return_refreshable_catalog_errors() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let response = handle_request(
            &state,
            &json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "grep_text",
                    "arguments": {"query": "needle", "path": "."}
                }
            }),
        );

        assert_eq!(response["error"]["data"]["error_code"], "UNKNOWN_TOOL");
        assert_eq!(response["error"]["data"]["retryable"], true);
        assert!(response["error"]["data"]["available_tools"]
            .as_array()
            .expect("available tools")
            .iter()
            .all(|tool| tool != "grep_text"));
    }

    #[tokio::test]
    async fn async_dispatch_routes_session_controls_without_blocking_workers() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let response = handle_request_async(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "wait_command",
                    "arguments": {"session_id": "missing", "timeout_ms": 1}
                }
            }),
        )
        .await;
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["execution_lane"], "async_control");
        assert_eq!(structured["blocking_queue_wait_ms"], 0);
    }

    #[tokio::test]
    async fn async_dispatch_keeps_lightweight_control_calls_inline() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let response = handle_request_async(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {"name": "server_info", "arguments": {}}
            }),
        )
        .await;
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["execution_lane"], "inline_fast");
        assert_eq!(structured["blocking_queue_wait_ms"], 0);
        assert_eq!(structured["admission_lane"], "fast");
        assert_eq!(structured["admission_limit"], 0);
        assert_eq!(structured["admission_queue_wait_ms"], 0);
    }

    #[tokio::test]
    async fn process_admission_queues_without_blocking_the_async_runtime() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let (_, _, admission, _, _) = state
            .admission_for("exec_command")
            .expect("process admission");
        let process_limit = admission.available_permits();
        assert!(process_limit > 0, "process admission must have capacity");
        let permits = admission
            .acquire_many_owned(process_limit as u32)
            .await
            .expect("reserve process lane");

        #[cfg(windows)]
        let command = "cmd /d /c echo admitted";
        #[cfg(unix)]
        let command = "sh -c \"printf admitted\"";

        let queued_state = state.clone();
        let command = command.to_string();
        let queued = tokio::spawn(async move {
            handle_request_async(
                queued_state,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "exec_command",
                        "arguments": {
                            "cmd": command,
                            "yield_time_ms": 5000,
                            "timeout_ms": 5000
                        }
                    }
                }),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!queued.is_finished(), "process request bypassed admission");

        let fast_started = Instant::now();
        let fast = handle_request_async(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "server_info", "arguments": {}}
            }),
        )
        .await;
        assert!(
            fast_started.elapsed() < Duration::from_millis(100),
            "{fast}"
        );
        assert_eq!(
            fast["result"]["structuredContent"]["execution_lane"],
            "inline_fast"
        );

        drop(permits);
        let response = queued.await.expect("queued tool task");
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["execution_lane"], "async_process");
        assert_eq!(structured["admission_lane"], "process");
        assert_eq!(structured["admission_limit"], json!(process_limit));
        assert!(
            structured["admission_queue_wait_ms"]
                .as_u64()
                .is_some_and(|wait| wait >= 80),
            "{structured}"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn async_exec_wait_does_not_block_fast_control_calls() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        #[cfg(windows)]
        let command =
            "powershell -NoProfile -Command \"Start-Sleep -Milliseconds 700; Write-Output done\"";
        #[cfg(unix)]
        let command = "sh -c \"sleep 0.7; printf done\"";

        let exec_state = state.clone();
        let exec = tokio::spawn(async move {
            handle_request_async(
                exec_state,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "exec_command",
                        "arguments": {
                            "cmd": command,
                            "yield_time_ms": 5000,
                            "timeout_ms": 5000,
                            "output_mode": "tail"
                        }
                    }
                }),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let fast_started = Instant::now();
        let fast = handle_request_async(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {"name": "server_info", "arguments": {}}
            }),
        )
        .await;
        assert!(
            fast_started.elapsed() < Duration::from_millis(500),
            "{fast}"
        );
        assert_eq!(
            fast["result"]["structuredContent"]["execution_lane"],
            "inline_fast"
        );

        let response = exec.await.expect("async exec task");
        let structured = &response["result"]["structuredContent"];
        assert_eq!(structured["execution_lane"], "async_process");
        assert_eq!(structured["blocking_queue_wait_ms"], 0);
        assert_eq!(structured["admission_lane"], "process");
        assert!(structured["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("done"));
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn cancelled_exec_request_terminates_the_orphaned_session() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let initial_slots = state.sessions.active_slots_available();

        #[cfg(windows)]
        let command = "powershell -NoProfile -Command \"Start-Sleep -Seconds 10\"";
        #[cfg(unix)]
        let command = "sh -c \"sleep 10\"";

        let request_state = state.clone();
        let request = tokio::spawn(async move {
            handle_request_async(
                request_state,
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "exec_command",
                        "arguments": {
                            "cmd": command,
                            "yield_time_ms": 5000,
                            "timeout_ms": 15_000,
                            "output_mode": "none"
                        }
                    }
                }),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            while state.sessions.active_slots_available() == initial_slots {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("exec session should start");
        assert_eq!(state.sessions.active_slots_available(), initial_slots - 1);

        request.abort();
        assert!(request
            .await
            .expect_err("request should be cancelled")
            .is_cancelled());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            state.sessions.active_slots_available(),
            initial_slots - 1,
            "detached session should remain attachable during the reconnect grace period"
        );

        tokio::time::timeout(Duration::from_secs(3), async {
            while state.sessions.active_slots_available() != initial_slots {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("cancelled request should release the active session slot");
    }

    #[tokio::test]
    async fn permission_resume_exec_uses_the_async_process_lane() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let mut context =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context");
        context.tool_profile = "guarded-core".into();
        let state = Arc::new(context);

        #[cfg(windows)]
        let shell = "powershell";
        #[cfg(windows)]
        let command = "Write-Output async-permission-resumed";
        #[cfg(unix)]
        let shell = "sh";
        #[cfg(unix)]
        let command = "printf async-permission-resumed";

        let blocked = handle_request_async(
            state.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "exec_command",
                    "arguments": {
                        "cmd": command,
                        "shell": shell,
                        "yield_time_ms": 5000,
                        "timeout_ms": 5000
                    }
                }
            }),
        )
        .await;
        let resume_id = blocked["result"]["structuredContent"]["error"]["details"]
            ["permission_request"]["resume_id"]
            .as_str()
            .expect("resume id")
            .to_string();

        let resumed = handle_request_async(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "request_permissions",
                    "arguments": {
                        "resume_id": resume_id,
                        "approve": true,
                        "confirm": true,
                        "scope": "once"
                    }
                }
            }),
        )
        .await;
        let structured = &resumed["result"]["structuredContent"];
        assert_eq!(
            structured["execution_lane"], "async_permission",
            "{structured}"
        );
        assert_eq!(structured["resumed_execution_lane"], "async_process");
        assert_eq!(structured["resumed"], true);
        assert_eq!(structured["command_ok"], true, "{structured}");
        assert!(structured["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("async-permission-resumed"));
    }

    #[tokio::test]
    async fn long_wait_does_not_block_unrelated_tool_calls() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        #[cfg(windows)]
        let command = "powershell -NoProfile -Command \"Start-Sleep -Milliseconds 900\"";
        #[cfg(unix)]
        let command = "sh -c \"sleep 0.9\"";

        let started = handle_request_async(
            state.clone(),
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": {
                    "name": "exec_command",
                    "arguments": {
                        "cmd": command,
                        "yield_time_ms": 0,
                        "timeout_ms": 5000,
                        "output_mode": "none"
                    }
                }
            }),
        )
        .await;
        let session_id = started["result"]["structuredContent"]["session_id"]
            .as_str()
            .expect("session id")
            .to_string();

        let wait_state = state.clone();
        let wait = tokio::spawn(async move {
            handle_request_async(
                wait_state,
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": {
                        "name": "wait_command",
                        "arguments": {
                            "session_id": session_id,
                            "timeout_ms": 700,
                            "output_mode": "none"
                        }
                    }
                }),
            )
            .await
        });
        tokio::time::sleep(Duration::from_millis(25)).await;

        let unrelated_started = Instant::now();
        let unrelated = handle_request_async(
            state,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "server_info", "arguments": {}}
            }),
        )
        .await;
        assert!(
            unrelated_started.elapsed() < Duration::from_millis(300),
            "{unrelated}"
        );
        assert_eq!(
            unrelated["result"]["structuredContent"]["execution_lane"],
            "inline_fast"
        );

        let waited = wait.await.expect("wait task");
        assert_eq!(
            waited["result"]["structuredContent"]["execution_lane"],
            "async_control"
        );
    }

    #[tokio::test]
    #[ignore = "manual runtime benchmark"]
    async fn runtime_benchmark_100_fast_lane_calls() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let started = Instant::now();
        let mut tasks = tokio::task::JoinSet::new();
        for id in 0..100 {
            let state = state.clone();
            tasks.spawn(async move {
                handle_request_async(
                    state,
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "method": "tools/call",
                        "params": {"name": "server_info", "arguments": {}}
                    }),
                )
                .await
            });
        }
        let mut completed = 0usize;
        while let Some(result) = tasks.join_next().await {
            let response = result.expect("fast task");
            assert_eq!(
                response["result"]["structuredContent"]["execution_lane"],
                "inline_fast"
            );
            completed += 1;
        }
        let elapsed = started.elapsed();
        println!(
            "{}",
            json!({
                "benchmark": "fast_lane_100",
                "completed": completed,
                "duration_ms": elapsed.as_millis(),
                "calls_per_second": completed as f64 / elapsed.as_secs_f64()
            })
        );
        assert_eq!(completed, 100);
    }

    #[tokio::test]
    #[ignore = "manual runtime benchmark"]
    async fn runtime_benchmark_16_active_process_sessions() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let state = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        #[cfg(windows)]
        let command = "powershell -NoProfile -Command \"Start-Sleep -Seconds 30\"";
        #[cfg(unix)]
        let command = "sh -c \"sleep 30\"";

        let started = Instant::now();
        let mut sessions = Vec::new();
        for id in 0..16 {
            let response = handle_request_async(
                state.clone(),
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": {
                        "name": "exec_command",
                        "arguments": {
                            "cmd": command,
                            "yield_time_ms": 0,
                            "timeout_ms": 60000,
                            "output_mode": "none"
                        }
                    }
                }),
            )
            .await;
            sessions.push(
                response["result"]["structuredContent"]["session_id"]
                    .as_str()
                    .expect("session id")
                    .to_string(),
            );
        }
        assert_eq!(state.sessions.active_slots_available(), 0);
        for (id, session_id) in sessions.into_iter().enumerate() {
            let killed = handle_request_async(
                state.clone(),
                json!({
                    "jsonrpc": "2.0",
                    "id": 100 + id,
                    "method": "tools/call",
                    "params": {
                        "name": "kill_session",
                        "arguments": {"session_id": session_id, "wait_ms": 10000}
                    }
                }),
            )
            .await;
            assert_eq!(killed["result"]["structuredContent"]["killed"], true);
        }
        println!(
            "{}",
            json!({
                "benchmark": "active_process_sessions_16",
                "duration_ms": started.elapsed().as_millis(),
                "active_slots_available": state.sessions.active_slots_available()
            })
        );
        assert_eq!(state.sessions.active_slots_available(), 16);
    }
}
