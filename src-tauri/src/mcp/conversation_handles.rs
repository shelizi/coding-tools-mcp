//! Authenticated, memory-only tool-argument conversation routing.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rand_core::{OsRng, RngCore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(crate) const KEY_PREFIX: &str = "ctmcp-handle:";
pub(crate) const TTL_MS: u64 = 24 * 60 * 60 * 1000;
pub(crate) const MAX_HANDLES: usize = 1024;

struct Entry {
    owner: String,
    native: Option<String>,
    key: String,
    expires_at: u64,
}

#[derive(Default)]
pub(crate) struct HandleStore {
    entries: HashMap<String, Entry>,
}

pub(crate) struct HandleResponse {
    token: Option<String>,
    expires_at: u64,
    platform_bound: bool,
}

fn digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn random_hex() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn error(code: &str, message: &str) -> Value {
    json!({"code": -32602, "message": message, "data": {
        "error_code": code, "error_category": "workspace_routing", "retryable": false
    }})
}

fn invalid() -> Value {
    error("CONVERSATION_HANDLE_INVALID", "Invalid conversation_handle for this authenticated credential and runtime; obtain a new handle with conversation_bootstrap.")
}

impl HandleStore {
    /// Expiry timestamps for the local management UI. Never expose handles or owners.
    pub(crate) fn expiry_times(&mut self, profile_id: &str, now: u64) -> Vec<u64> {
        self.entries.retain(|_, entry| {
            if entry.expires_at > now {
                return true;
            }
            if entry.native.is_none() {
                crate::tools::hub::forget_handle_conversation(profile_id, &entry.key);
            }
            false
        });
        let mut times: Vec<_> = self
            .entries
            .values()
            .map(|entry| entry.expires_at)
            .collect();
        times.sort_unstable();
        times
    }

    fn prepare(
        &mut self,
        profile_id: &str,
        owner: Option<&str>,
        body: &mut Value,
        now: u64,
    ) -> Result<Option<HandleResponse>, Value> {
        let native = body
            .pointer("/params/_meta/openai~1session")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_owned);
        if native
            .as_deref()
            .is_some_and(|key| key.starts_with(KEY_PREFIX))
        {
            return Err(error(
                "CONVERSATION_HANDLE_CONFLICT",
                "Reserved conversation identity namespace.",
            ));
        }
        if body.get("method").and_then(Value::as_str) != Some("tools/call") {
            return Ok(None);
        }
        let name = body
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let raw = body
            .pointer_mut("/params/arguments")
            .and_then(Value::as_object_mut)
            .and_then(|args| args.remove("conversation_handle"));
        let create = body
            .pointer_mut("/params/arguments")
            .and_then(Value::as_object_mut)
            .and_then(|args| args.remove("create_conversation_handle"));
        if let Some(create) = &create {
            if name != "conversation_bootstrap" || !create.is_boolean() {
                return Err(error("INVALID_ARGUMENT", "create_conversation_handle is a boolean supported only by conversation_bootstrap."));
            }
        }
        let mint = raw.is_none()
            && name == "conversation_bootstrap"
            && (native.is_none() || create == Some(Value::Bool(true)));
        if raw.is_none() && !mint {
            return Ok(None);
        }
        if !crate::tools::registry::is_allowed_tool(crate::tools::registry::canonical_tool_name(
            &name,
        )) {
            return Err(error(
                "INVALID_ARGUMENT",
                "conversation_handle is supported only by built-in MCP tools.",
            ));
        }
        let owner = owner.ok_or_else(|| {
            error(
                "CONVERSATION_HANDLE_AUTH_REQUIRED",
                "conversation_handle requires verified Bearer authentication.",
            )
        })?;
        let (token, key, expires_at, platform_bound) = if let Some(raw) = raw {
            let token = raw
                .as_str()
                .filter(|value| {
                    value.len() == 67
                        && value.starts_with("ch_")
                        && value[3..]
                            .bytes()
                            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                })
                .ok_or_else(invalid)?
                .to_owned();
            let id = digest(&token);
            let entry = self.entries.get(&id).ok_or_else(invalid)?;
            if entry.owner != owner {
                return Err(invalid());
            }
            if entry.native != native {
                return Err(error("CONVERSATION_HANDLE_CONFLICT", "conversation_handle does not match the platform conversation identity used at issuance."));
            }
            if entry.expires_at <= now {
                let entry = self.entries.remove(&id).expect("expired entry");
                if entry.native.is_none() {
                    crate::tools::hub::forget_handle_conversation(profile_id, &entry.key);
                }
                return Err(error(
                    "CONVERSATION_HANDLE_EXPIRED",
                    "conversation_handle expired; obtain a new handle with conversation_bootstrap.",
                ));
            }
            (
                token,
                entry.key.clone(),
                entry.expires_at,
                entry.native.is_some(),
            )
        } else {
            self.entries.retain(|_, entry| {
                if entry.expires_at > now {
                    return true;
                }
                if entry.native.is_none() {
                    crate::tools::hub::forget_handle_conversation(profile_id, &entry.key);
                }
                false
            });
            if self.entries.len() >= MAX_HANDLES {
                return Err(error("CONVERSATION_HANDLE_LIMIT", "Conversation handle capacity reached; reuse an existing handle or wait for expiry."));
            }
            let token = format!("ch_{}", random_hex());
            let key = native
                .clone()
                .unwrap_or_else(|| format!("{KEY_PREFIX}{}", random_hex()));
            let expires_at = now.saturating_add(TTL_MS);
            self.entries.insert(
                digest(&token),
                Entry {
                    owner: owner.into(),
                    native: native.clone(),
                    key: key.clone(),
                    expires_at,
                },
            );
            (token, key, expires_at, native.is_some())
        };
        let params = body
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| error("INVALID_ARGUMENT", "Tool params must be an object."))?;
        let meta = params.entry("_meta").or_insert_with(|| json!({}));
        if !meta.is_object() {
            *meta = json!({});
        }
        meta["openai/session"] = json!(key);
        if !platform_bound {
            if let Some(extensions) = meta
                .pointer_mut("/io.modelcontextprotocol~1clientCapabilities/extensions")
                .and_then(Value::as_object_mut)
            {
                extensions.remove("io.modelcontextprotocol/tasks");
            }
        }
        Ok(Some(HandleResponse {
            token: (name == "conversation_bootstrap").then_some(token),
            expires_at,
            platform_bound,
        }))
    }
}

/// Must only be called after HTTP authentication has succeeded. `credential` is
/// supplied by the verified transport boundary, never by JSON metadata/arguments.
pub(crate) fn prepare(
    store: &Mutex<HandleStore>,
    profile_id: &str,
    credential: Option<&str>,
    body: &mut Value,
) -> Result<Option<HandleResponse>, Value> {
    let owner = credential.map(digest);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let mut store = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    store.prepare(profile_id, owner.as_deref(), body, now)
}

impl HandleResponse {
    /// Runs after tool execution and its telemetry. Raw handles must never be
    /// present in execution arguments, history, deduplication or usage records.
    pub(crate) fn decorate(self, mut response: Value) -> Value {
        let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
            return response;
        };
        let mut fields = json!({
            "conversation_identity_source": "conversation_handle",
            "platform_conversation_bound": self.platform_bound,
            "conversation_handle_expires_at_ms": self.expires_at
        });
        if let Some(token) = self.token {
            fields["conversation_handle"] = json!(token);
        }
        let structured = result
            .get("structuredContent")
            .and_then(Value::as_object)
            .cloned();
        if structured.is_none() && !result.get("content").is_some_and(Value::is_array) {
            return response;
        }
        let content = result
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut payload = structured.unwrap_or_else(|| {
            let text = content
                .iter()
                .filter(|block| block["type"] == "text")
                .filter_map(|block| block["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                serde_json::Map::new()
            } else {
                json!({"text":text}).as_object().unwrap().clone()
            }
        });
        payload.extend(fields.as_object().expect("fields").clone());
        let payload = Value::Object(payload);
        // Clients that consume only one text block must still receive tool data.
        let mut merged = vec![json!({"type":"text", "text":payload.to_string()})];
        merged.extend(content.into_iter().filter(|block| block["type"] != "text"));
        result.insert("structuredContent".into(), payload);
        result.insert("content".into(), Value::Array(merged));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bootstrap(native: Option<&str>) -> Value {
        let mut body = json!({"method": "tools/call", "params": {"name": "conversation_bootstrap", "arguments": {"create_conversation_handle": true}}});
        if let Some(native) = native {
            body["params"]["_meta"] = json!({"openai/session": native});
        }
        body
    }

    #[test]
    fn handles_are_random_owner_bound_native_bound_and_absolutely_expiring() {
        let mut store = HandleStore::default();
        let mut first = bootstrap(None);
        let grant = store
            .prepare("test-profile", Some("owner"), &mut first, 100)
            .unwrap()
            .unwrap();
        let token = grant.token.unwrap();
        assert_eq!(token.len(), 67);
        assert!(first["params"]["arguments"]
            .get("create_conversation_handle")
            .is_none());
        let original_key = first["params"]["_meta"]["openai/session"].clone();
        let request = json!({"method":"tools/call", "params":{"name":"list_workspace_folders", "arguments":{"conversation_handle":token}}});
        let mut reuse = request.clone();
        store
            .prepare("test-profile", Some("owner"), &mut reuse, 200)
            .unwrap();
        assert_eq!(reuse["params"]["_meta"]["openai/session"], original_key);
        assert!(store
            .prepare("test-profile", Some("different"), &mut request.clone(), 200)
            .is_err());
        let mut conflict = request.clone();
        conflict["params"]["_meta"] = json!({"openai/session":"other-chat"});
        assert_eq!(
            store
                .prepare("test-profile", Some("owner"), &mut conflict, 200)
                .err()
                .unwrap()["data"]["error_code"],
            "CONVERSATION_HANDLE_CONFLICT"
        );
        assert_eq!(
            store
                .prepare(
                    "test-profile",
                    Some("owner"),
                    &mut request.clone(),
                    100 + TTL_MS
                )
                .err()
                .unwrap()["data"]["error_code"],
            "CONVERSATION_HANDLE_EXPIRED"
        );
        let mut second = bootstrap(None);
        let other = store
            .prepare("test-profile", Some("owner"), &mut second, 200 + TTL_MS)
            .unwrap()
            .unwrap();
        assert_ne!(other.token.unwrap(), token);
        assert_ne!(second["params"]["_meta"]["openai/session"], original_key);
        assert!(HandleStore::default()
            .prepare("test-profile", Some("owner"), &mut request.clone(), 200)
            .is_err());
    }

    #[test]
    fn native_flow_auth_and_namespace_are_guarded() {
        let mut store = HandleStore::default();
        assert!(store
            .prepare("test-profile", None, &mut bootstrap(None), 0)
            .is_err());
        let mut native = bootstrap(Some("chatgpt"));
        native["params"]["arguments"] = json!({});
        assert!(store
            .prepare("test-profile", None, &mut native, 0)
            .unwrap()
            .is_none());
        assert_eq!(native["params"]["_meta"]["openai/session"], "chatgpt");
        let mut requested = bootstrap(Some("chatgpt"));
        let grant = store
            .prepare("test-profile", Some("owner"), &mut requested, 0)
            .unwrap()
            .unwrap();
        assert!(grant.platform_bound);
        let mut request = bootstrap(None);
        request["params"]["arguments"] = json!({"conversation_handle": grant.token.unwrap()});
        assert!(store
            .prepare("test-profile", Some("owner"), &mut request, 0)
            .is_err());
        assert!(store
            .prepare(
                "test-profile",
                Some("owner"),
                &mut bootstrap(Some("ctmcp-handle:spoof")),
                0
            )
            .is_err());
    }

    #[test]
    fn capacity_is_bounded_and_expiry_releases_capacity() {
        let mut store = HandleStore::default();
        for _ in 0..MAX_HANDLES {
            store
                .prepare("capacity-profile", Some("owner"), &mut bootstrap(None), 0)
                .unwrap();
        }
        assert_eq!(
            store
                .prepare("capacity-profile", Some("owner"), &mut bootstrap(None), 1)
                .err()
                .unwrap()["data"]["error_code"],
            "CONVERSATION_HANDLE_LIMIT"
        );
        assert!(store
            .prepare(
                "capacity-profile",
                Some("owner"),
                &mut bootstrap(None),
                TTL_MS
            )
            .unwrap()
            .is_some());
        assert_eq!(store.entries.len(), 1);
    }

    #[test]
    fn decorated_results_keep_complete_data_in_one_text_block_and_preserve_media_and_errors() {
        let image = json!({"type":"image","mimeType":"image/png","data":"aW1hZ2U="});
        let resource = json!({"type":"resource_link","uri":"output://example","name":"output"});
        for structured in [
            json!({"ok":true,"files":["one.txt","two.txt"]}),
            json!({"ok":false,"error":"permission denied"}),
        ] {
            let is_error = structured["ok"] == false;
            let result = HandleResponse {token:None,expires_at:1234,platform_bound:false}.decorate(json!({"result":{
                "structuredContent":structured,"isError":is_error,"content":[{"type":"text","text":"summary"},image,{"type":"text","text":"old metadata"},resource]
            }}));
            let result = &result["result"];
            assert_eq!(
                result["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|block| block["type"] == "text")
                    .count(),
                1
            );
            let text: Value =
                serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
            assert_eq!(text, result["structuredContent"]);
            assert_eq!(text["ok"], structured["ok"]);
            assert_eq!(text["files"], structured["files"]);
            assert_eq!(text["error"], structured["error"]);
            assert_eq!(text["conversation_handle_expires_at_ms"], 1234);
            assert_eq!(result["content"][1], image);
            assert_eq!(result["content"][2], resource);
            assert_eq!(result["isError"], is_error);
        }
        let legacy = HandleResponse {
            token: None,
            expires_at: 1234,
            platform_bound: false,
        }
        .decorate(
            json!({"result":{"isError":true,"content":[{"type":"text","text":"original error"}]}}),
        );
        let text: Value =
            serde_json::from_str(legacy["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["text"], "original error");
        assert_eq!(legacy["result"]["isError"], true);
        let task = json!({"result":{"resultType":"task","task":{"taskId":"retained-task"}}});
        assert_eq!(
            HandleResponse {
                token: None,
                expires_at: 1234,
                platform_bound: true
            }
            .decorate(task.clone()),
            task
        );
    }

    #[test]
    fn private_dispatch_removes_handle_arguments_and_only_suppresses_identityless_tasks() {
        let mut store = HandleStore::default();
        for native in [None, Some("native-chat")] {
            let mut request = bootstrap(native);
            request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"] = json!({
                "elicitation": {}, "extensions": {"io.modelcontextprotocol/tasks": {}, "other": {}}
            });
            let grant = store
                .prepare("test-profile", Some("owner"), &mut request, 0)
                .unwrap()
                .unwrap();
            assert!(request["params"]["arguments"]
                .as_object()
                .unwrap()
                .is_empty());
            let capabilities =
                &request["params"]["_meta"]["io.modelcontextprotocol/clientCapabilities"];
            assert!(capabilities.get("elicitation").is_some());
            assert!(capabilities["extensions"].get("other").is_some());
            assert_eq!(
                capabilities["extensions"]
                    .get("io.modelcontextprotocol/tasks")
                    .is_some(),
                native.is_some()
            );
            let token = grant.token.as_deref().unwrap();
            assert!(!store.entries.contains_key(token));
            let response =
                grant.decorate(json!({"result":{"structuredContent":{"ok":true},"content":[]}}));
            assert!(response["result"]["structuredContent"]["conversation_handle"].is_string());
        }
        for raw in [
            Value::Null,
            json!(12),
            json!(true),
            json!({}),
            json!("ch_bad"),
        ] {
            let mut request = json!({"method":"tools/call","params":{"name":"read_file","arguments":{"conversation_handle":raw,"workspace_folder_id":"explicit"}}});
            assert_eq!(
                store
                    .prepare("test-profile", Some("owner"), &mut request, 0)
                    .err()
                    .unwrap()["data"]["error_code"],
                "CONVERSATION_HANDLE_INVALID"
            );
        }
    }
}
