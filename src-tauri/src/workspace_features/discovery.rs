use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::workspace::WorkspaceFolder;

const MAX_EXTENSION_CONFIG_BYTES: u64 = 1024 * 1024;
const SUPPORTED_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionDiagnostic {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HookDescriptor {
    pub key: String,
    pub provider: String,
    pub scope: String,
    pub folder_id: Option<String>,
    pub event: String,
    pub matcher: Option<String>,
    pub handler_type: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub timeout_ms: u64,
    pub source_path: String,
    pub source_enabled: bool,
    pub supported: bool,
}

#[derive(Debug, Clone)]
pub struct McpServerDescriptor {
    pub key: String,
    pub provider: String,
    pub scope: String,
    pub folder_id: String,
    pub name: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
    pub env_vars: Vec<String>,
    pub cwd: Option<String>,
    pub url: Option<String>,
    pub headers: HashMap<String, String>,
    pub env_headers: HashMap<String, String>,
    pub bearer_token_env_var: Option<String>,
    pub source_path: String,
    pub source_enabled: bool,
    pub supported: bool,
}

pub struct DiscoveredExtensions {
    pub hooks: Vec<HookDescriptor>,
    pub mcp_servers: Vec<McpServerDescriptor>,
    pub diagnostics: Vec<ExtensionDiagnostic>,
}

fn object(value: &Value) -> &Map<String, Value> {
    value.as_object().unwrap_or_else(|| {
        static EMPTY: std::sync::OnceLock<Map<String, Value>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(Map::new)
    })
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .or_else(|| Some(item.to_string()))
                })
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn string_record(value: Option<&Value>) -> HashMap<String, String> {
    value
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| value.to_string()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn sanitize(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut in_invalid_run = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            result.push(ch);
            in_invalid_run = false;
        } else if !in_invalid_run {
            result.push('_');
            in_invalid_run = true;
        }
    }
    result
}

fn descriptor_key(parts: &[Value]) -> String {
    let prefix = parts
        .iter()
        .take(4)
        .map(|value| match value {
            Value::Null => String::new(),
            Value::String(value) => value.clone(),
            _ => value.to_string(),
        })
        .map(|value| sanitize(&value))
        .collect::<Vec<_>>()
        .join(":");
    let prefix = prefix.chars().take(80).collect::<String>();
    let encoded = serde_json::to_vec(parts).unwrap_or_default();
    let digest = format!("{:x}", Sha256::digest(encoded));
    format!("{prefix}:{}", &digest[..16])
}

fn safe_read(
    file: &Path,
    containment_root: &Path,
    display: &str,
    diagnostics: &mut Vec<ExtensionDiagnostic>,
    provider: &str,
    scope: &str,
) -> Option<String> {
    let metadata = match fs::symlink_metadata(file) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            diagnostics.push(ExtensionDiagnostic {
                code: "EXTENSION_CONFIG_READ_FAILED".into(),
                message: format!("Failed to read extension configuration ({}).", error.kind()),
                provider: Some(provider.into()),
                scope: Some(scope.into()),
                path: Some(display.into()),
                key: None,
            });
            return None;
        }
    };
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        diagnostics.push(ExtensionDiagnostic {
            code: "EXTENSION_CONFIG_SKIPPED".into(),
            message: "Extension config must be a regular non-symlink file.".into(),
            provider: Some(provider.into()),
            scope: Some(scope.into()),
            path: Some(display.into()),
            key: None,
        });
        return None;
    }
    if metadata.len() > MAX_EXTENSION_CONFIG_BYTES {
        diagnostics.push(ExtensionDiagnostic {
            code: "EXTENSION_CONFIG_TOO_LARGE".into(),
            message: format!("Extension config exceeds {MAX_EXTENSION_CONFIG_BYTES} bytes."),
            provider: Some(provider.into()),
            scope: Some(scope.into()),
            path: Some(display.into()),
            key: None,
        });
        return None;
    }
    let resolved = match file.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) => {
            diagnostics.push(ExtensionDiagnostic {
                code: "EXTENSION_CONFIG_READ_FAILED".into(),
                message: error.to_string(),
                provider: Some(provider.into()),
                scope: Some(scope.into()),
                path: Some(display.into()),
                key: None,
            });
            return None;
        }
    };
    if !resolved.starts_with(containment_root) {
        diagnostics.push(ExtensionDiagnostic {
            code: "EXTENSION_CONFIG_OUTSIDE_SCOPE".into(),
            message: "Resolved extension config escapes its allowed scope.".into(),
            provider: Some(provider.into()),
            scope: Some(scope.into()),
            path: Some(display.into()),
            key: None,
        });
        return None;
    }
    match fs::read_to_string(resolved) {
        Ok(content) => Some(content),
        Err(error) => {
            diagnostics.push(ExtensionDiagnostic {
                code: "EXTENSION_CONFIG_READ_FAILED".into(),
                message: error.to_string(),
                provider: Some(provider.into()),
                scope: Some(scope.into()),
                path: Some(display.into()),
                key: None,
            });
            None
        }
    }
}

fn read_json(
    file: &Path,
    containment_root: &Path,
    display: &str,
    diagnostics: &mut Vec<ExtensionDiagnostic>,
    provider: &str,
    scope: &str,
) -> Value {
    let Some(content) = safe_read(
        file,
        containment_root,
        display,
        diagnostics,
        provider,
        scope,
    ) else {
        return Value::Object(Map::new());
    };
    match serde_json::from_str::<Value>(&content) {
        Ok(value) if value.is_object() => value,
        _ => {
            diagnostics.push(ExtensionDiagnostic {
                code: "EXTENSION_CONFIG_INVALID_JSON".into(),
                message: "Invalid JSON extension configuration.".into(),
                provider: Some(provider.into()),
                scope: Some(scope.into()),
                path: Some(display.into()),
                key: None,
            });
            Value::Object(Map::new())
        }
    }
}

fn timeout_ms(handler: &Map<String, Value>) -> u64 {
    let explicit = handler
        .get("timeout_ms")
        .or_else(|| handler.get("timeoutMs"))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| value.round() as u64);
    if let Some(explicit) = explicit {
        return explicit.min(120_000);
    }
    handler
        .get("timeout")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0)
        .map(|value| (value * 1000.0).round() as u64)
        .unwrap_or(10_000)
        .min(120_000)
}

fn hooks_from_document(
    document: &Value,
    provider: &str,
    scope: &str,
    folder_id: Option<&str>,
    source_path: &str,
    source_enabled: bool,
    diagnostics: &mut Vec<ExtensionDiagnostic>,
) -> Vec<HookDescriptor> {
    let mut result = Vec::new();
    let Some(hooks) = document.get("hooks").and_then(Value::as_object) else {
        return result;
    };
    for (event, raw_groups) in hooks {
        let groups = raw_groups
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![raw_groups.clone()]);
        for (group_index, raw_group) in groups.iter().enumerate() {
            let group = object(raw_group);
            let matcher = group
                .get("matcher")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let handlers = group
                .get("hooks")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| {
                    if group.contains_key("command") || group.contains_key("type") {
                        vec![raw_group.clone()]
                    } else {
                        Vec::new()
                    }
                });
            for (handler_index, raw_handler) in handlers.iter().enumerate() {
                let handler = object(raw_handler);
                let handler_type = handler
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("command")
                    .trim()
                    .to_ascii_lowercase();
                let command = handler
                    .get("command")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let url = handler
                    .get("url")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let handler_supported = match handler_type.as_str() {
                    "command" => command.is_some(),
                    "http" => url.is_some(),
                    _ => false,
                };
                let event_supported = SUPPORTED_HOOK_EVENTS.contains(&event.as_str());
                let supported = handler_supported && event_supported;
                let parts = vec![
                    Value::String("hook".into()),
                    Value::String(provider.into()),
                    Value::String(scope.into()),
                    Value::String(folder_id.unwrap_or_default().into()),
                    Value::String(source_path.into()),
                    Value::String(event.clone()),
                    Value::from(group_index),
                    Value::from(handler_index),
                    matcher.clone().map(Value::String).unwrap_or(Value::Null),
                    Value::String(handler_type.clone()),
                    command.clone().map(Value::String).unwrap_or(Value::Null),
                    url.clone().map(Value::String).unwrap_or(Value::Null),
                ];
                let key = descriptor_key(&parts);
                if !event_supported {
                    diagnostics.push(ExtensionDiagnostic {
                        code: "HOOK_EVENT_UNSUPPORTED".into(),
                        message: format!(
                            "Hook event {} is discoverable but not executable by Rust Desktop.",
                            if event.is_empty() { "unknown" } else { event }
                        ),
                        provider: Some(provider.into()),
                        scope: Some(scope.into()),
                        path: Some(source_path.into()),
                        key: Some(key.clone()),
                    });
                } else if !handler_supported {
                    diagnostics.push(ExtensionDiagnostic {
                        code: "HOOK_HANDLER_UNSUPPORTED".into(),
                        message: format!(
                            "Hook handler type {} is discoverable but not executable by Rust Desktop.",
                            handler_type
                        ),
                        provider: Some(provider.into()),
                        scope: Some(scope.into()),
                        path: Some(source_path.into()),
                        key: Some(key.clone()),
                    });
                }
                result.push(HookDescriptor {
                    key,
                    provider: provider.into(),
                    scope: scope.into(),
                    folder_id: folder_id.map(str::to_string),
                    event: event.clone(),
                    matcher: matcher.clone(),
                    handler_type,
                    command,
                    args: string_array(handler.get("args")),
                    url,
                    timeout_ms: timeout_ms(handler),
                    source_path: source_path.into(),
                    source_enabled,
                    supported,
                });
            }
        }
    }
    result
}

fn expand_env(value: &str) -> String {
    let regex = Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(?::-([^}]*))?\}").expect("env regex");
    regex
        .replace_all(value, |captures: &regex::Captures<'_>| {
            let name = captures
                .get(1)
                .map(|value| value.as_str())
                .unwrap_or_default();
            std::env::var(name).unwrap_or_else(|_| {
                captures
                    .get(2)
                    .map(|value| value.as_str().to_string())
                    .unwrap_or_default()
            })
        })
        .into_owned()
}

fn mcp_transport(config: &Map<String, Value>) -> String {
    let explicit = config
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if matches!(explicit.as_str(), "stdio" | "http" | "sse" | "ws") {
        return explicit;
    }
    if config.contains_key("command") {
        "stdio".into()
    } else if config.contains_key("url") {
        "http".into()
    } else {
        "unknown".into()
    }
}

fn mcp_descriptor(
    provider: &str,
    scope: &str,
    folder_id: &str,
    source_path: &str,
    name: &str,
    raw: &Value,
    source_enabled: bool,
) -> McpServerDescriptor {
    let config = object(raw);
    let transport = mcp_transport(config);
    let command = config
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let url = config
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(expand_env);
    let env = string_record(config.get("env"))
        .into_iter()
        .map(|(key, value)| (key, expand_env(&value)))
        .collect();
    let headers = string_record(config.get("http_headers").or_else(|| config.get("headers")))
        .into_iter()
        .map(|(key, value)| (key, expand_env(&value)))
        .collect();
    let key = descriptor_key(&[
        Value::String("mcp".into()),
        Value::String(provider.into()),
        Value::String(scope.into()),
        Value::String(folder_id.into()),
        Value::String(source_path.into()),
        Value::String(name.into()),
    ]);
    let config_enabled = config.get("enabled").and_then(Value::as_bool) != Some(false);
    let supported = match transport.as_str() {
        "stdio" => command.is_some(),
        "http" => url.is_some(),
        _ => false,
    };
    McpServerDescriptor {
        key,
        provider: provider.into(),
        scope: scope.into(),
        folder_id: folder_id.into(),
        name: name.into(),
        transport,
        command,
        args: string_array(config.get("args"))
            .into_iter()
            .map(|value| expand_env(&value))
            .collect(),
        env,
        env_vars: string_array(config.get("env_vars").or_else(|| config.get("envVars"))),
        cwd: config
            .get("cwd")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        url,
        headers,
        env_headers: string_record(
            config
                .get("env_http_headers")
                .or_else(|| config.get("envHeaders")),
        ),
        bearer_token_env_var: config
            .get("bearer_token_env_var")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        source_path: source_path.into(),
        source_enabled: source_enabled && config_enabled,
        supported,
    }
}

fn collect_mcp(
    target: &mut Vec<McpServerDescriptor>,
    provider: &str,
    scope: &str,
    folder_id: &str,
    source_path: &str,
    servers: Option<&Value>,
    source_enabled: bool,
    diagnostics: &mut Vec<ExtensionDiagnostic>,
) {
    let Some(servers) = servers.and_then(Value::as_object) else {
        return;
    };
    for (name, raw) in servers {
        let server = mcp_descriptor(
            provider,
            scope,
            folder_id,
            source_path,
            name,
            raw,
            source_enabled,
        );
        if !server.supported {
            diagnostics.push(ExtensionDiagnostic {
                code: "MCP_TRANSPORT_UNSUPPORTED".into(),
                message: format!(
                    "MCP server {name} uses unsupported or incomplete transport {}. Rust Desktop currently proxies stdio and streamable HTTP.",
                    server.transport
                ),
                provider: Some(provider.into()),
                scope: Some(scope.into()),
                path: Some(source_path.into()),
                key: Some(server.key.clone()),
            });
        }
        target.push(server);
    }
}

fn precedence(scope: &str) -> usize {
    match scope {
        "local" => 0,
        "workspace" => 10,
        _ => 20,
    }
}

fn dedupe_mcp(
    mut candidates: Vec<McpServerDescriptor>,
    diagnostics: &mut Vec<ExtensionDiagnostic>,
) -> Vec<McpServerDescriptor> {
    candidates.sort_by(|left, right| {
        precedence(&left.scope)
            .cmp(&precedence(&right.scope))
            .then_with(|| left.source_path.cmp(&right.source_path))
    });
    let mut selected = HashMap::<String, McpServerDescriptor>::new();
    for server in candidates {
        let identity = format!(
            "{}:{}:{}",
            server.provider,
            server.folder_id,
            server.name.to_ascii_lowercase()
        );
        if let Some(existing) = selected.get(&identity) {
            diagnostics.push(ExtensionDiagnostic {
                code: "MCP_SERVER_SHADOWED".into(),
                message: format!(
                    "{} server {} is shadowed by {}.",
                    server.source_path, server.name, existing.source_path
                ),
                provider: Some(server.provider.clone()),
                scope: Some(server.scope.clone()),
                path: Some(server.source_path.clone()),
                key: Some(server.key.clone()),
            });
        } else {
            selected.insert(identity, server);
        }
    }
    let mut result = selected.into_values().collect::<Vec<_>>();
    result.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.key.cmp(&right.key))
    });
    result
}

pub async fn discover_extensions(folders: &[WorkspaceFolder]) -> DiscoveredExtensions {
    let mut diagnostics = Vec::new();
    let mut hooks = Vec::new();
    let mut mcp_candidates = Vec::new();
    let home = dirs::home_dir();
    let home_real = home.as_ref().and_then(|path| path.canonicalize().ok());
    let mut user_mcp = Value::Object(Map::new());

    // Only Coding MCP's own configurations are read; other Agents are never scanned.
    if let (Some(home), Some(home_real)) = (&home, &home_real) {
        user_mcp = read_json(
            &home.join(".coding-tools-mcp/mcp.json"),
            home_real,
            "~/.coding-tools-mcp/mcp.json",
            &mut diagnostics,
            "coding-mcp",
            "user",
        );
        let user_hooks = read_json(
            &home.join(".coding-tools-mcp/hooks.json"),
            home_real,
            "~/.coding-tools-mcp/hooks.json",
            &mut diagnostics,
            "coding-mcp",
            "user",
        );
        hooks.extend(hooks_from_document(
            &user_hooks,
            "coding-mcp",
            "user",
            None,
            "~/.coding-tools-mcp/hooks.json",
            true,
            &mut diagnostics,
        ));
    }

    for folder in folders {
        let workspace = PathBuf::from(&folder.path);
        let workspace_real = match workspace.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                diagnostics.push(ExtensionDiagnostic {
                    code: "EXTENSION_WORKSPACE_FAILED".into(),
                    message: error.to_string(),
                    provider: None,
                    scope: Some("workspace".into()),
                    path: Some(folder.path.clone()),
                    key: None,
                });
                continue;
            }
        };

        if home_real.is_some() {
            collect_mcp(
                &mut mcp_candidates,
                "coding-mcp",
                "user",
                &folder.id,
                "~/.coding-tools-mcp/mcp.json",
                user_mcp.get("mcpServers"),
                true,
                &mut diagnostics,
            );
        }

        let workspace_mcp = read_json(
            &workspace.join(".coding-tools-mcp/mcp.json"),
            &workspace_real,
            ".coding-tools-mcp/mcp.json",
            &mut diagnostics,
            "coding-mcp",
            "workspace",
        );
        collect_mcp(
            &mut mcp_candidates,
            "coding-mcp",
            "workspace",
            &folder.id,
            ".coding-tools-mcp/mcp.json",
            workspace_mcp.get("mcpServers"),
            true,
            &mut diagnostics,
        );

        let workspace_hooks = read_json(
            &workspace.join(".coding-tools-mcp/hooks.json"),
            &workspace_real,
            ".coding-tools-mcp/hooks.json",
            &mut diagnostics,
            "coding-mcp",
            "workspace",
        );
        hooks.extend(hooks_from_document(
            &workspace_hooks,
            "coding-mcp",
            "workspace",
            Some(&folder.id),
            ".coding-tools-mcp/hooks.json",
            true,
            &mut diagnostics,
        ));
    }

    hooks.sort_by(|left, right| {
        left.event
            .cmp(&right.event)
            .then_with(|| left.key.cmp(&right.key))
    });
    DiscoveredExtensions {
        hooks,
        mcp_servers: dedupe_mcp(mcp_candidates, &mut diagnostics),
        diagnostics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn descriptor_keys_match_node_hashing_and_invalid_run_collapse() {
        let parts = vec![
            Value::String("hook".into()),
            Value::String("codex".into()),
            Value::String("workspace".into()),
            Value::String("folder id".into()),
            Value::String("path".into()),
            Value::String("PreToolUse".into()),
            Value::from(0),
            Value::from(0),
            Value::String("read file".into()),
            Value::String("command".into()),
            Value::String("echo ok".into()),
            Value::Null,
        ];
        assert_eq!(
            descriptor_key(&parts),
            "hook:codex:workspace:folder_id:45c8b22229ba566a"
        );
        assert_eq!(sanitize("a  /  b"), "a_b");
    }

    #[tokio::test]
    async fn discovers_only_coding_mcp_owned_workspace_servers_and_hooks() {
        let root = tempdir().expect("tempdir");
        fs::create_dir_all(root.path().join(".coding-tools-mcp")).expect("own config dir");
        fs::write(
            root.path().join(".coding-tools-mcp/mcp.json"),
            r#"{"mcpServers":{"demo":{"command":"demo-server"}}}"#,
        )
        .expect("mcp json");
        fs::write(
            root.path().join(".coding-tools-mcp/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"read_file","hooks":[{"type":"command","command":"echo ok"}]}]}}"#,
        )
        .expect("hooks");
        // Existing configs from other Agents must not be scanned or shadow our own.
        fs::write(
            root.path().join(".mcp.json"),
            r#"{"mcpServers":{"foreign":{"command":"not-ours"}}}"#,
        )
        .expect("legacy mcp json");
        fs::create_dir_all(root.path().join(".codex")).expect("codex dir");
        fs::write(
            root.path().join(".codex/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"foreign-hook"}]}]}}"#,
        )
        .expect("foreign hooks");
        let folder = WorkspaceFolder::new(
            root.path().to_string_lossy().into_owned(),
            Some("root".into()),
        );
        let folder_id = folder.id.clone();
        let found = discover_extensions(&[folder]).await;
        let demo = found
            .mcp_servers
            .iter()
            .find(|server| server.name == "demo" && server.folder_id == folder_id)
            .expect("workspace own MCP server");
        assert_eq!(demo.provider, "coding-mcp");
        assert_eq!(demo.scope, "workspace");
        assert!(found
            .mcp_servers
            .iter()
            .all(|server| server.name != "foreign"));
        assert!(found.hooks.iter().any(|hook| {
            hook.event == "PreToolUse"
                && hook.provider == "coding-mcp"
                && hook.scope == "workspace"
                && hook.folder_id.as_deref() == Some(folder_id.as_str())
        }));
        assert!(found
            .hooks
            .iter()
            .all(|hook| hook.command.as_deref() != Some("foreign-hook")));
    }
}
