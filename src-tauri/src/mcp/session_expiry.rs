//! Read-only, process-local expiry snapshots for the authenticated desktop UI.
//! A snapshot never contains raw tokens, owners, session keys, or token hashes.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};

use super::conversation_handles::HandleStore;
use crate::auth::OAuthRuntime;

#[derive(Clone)]
struct LiveSessionExpiry {
    handles: Arc<Mutex<HandleStore>>,
    oauth: Option<Arc<OAuthRuntime>>,
    auth_type: String,
}

static LIVE: OnceLock<Mutex<HashMap<String, LiveSessionExpiry>>> = OnceLock::new();

fn registry() -> &'static Mutex<HashMap<String, LiveSessionExpiry>> {
    LIVE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn register(
    workspace_id: &str,
    auth_type: &str,
    handles: Arc<Mutex<HandleStore>>,
    oauth: Option<Arc<OAuthRuntime>>,
) {
    let mut entries = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    entries.insert(
        workspace_id.to_owned(),
        LiveSessionExpiry {
            handles,
            oauth,
            auth_type: auth_type.to_owned(),
        },
    );
}

pub(crate) fn unregister(workspace_id: &str, handles: &Arc<Mutex<HandleStore>>) {
    let mut entries = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if entries
        .get(workspace_id)
        .is_some_and(|item| Arc::ptr_eq(&item.handles, handles))
    {
        entries.remove(workspace_id);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SessionExpirySnapshot {
    pub captured_at_ms: u64,
    pub runtime_active: bool,
    pub auth_type: String,
    pub oauth_token_ttl_seconds: Option<u64>,
    pub oauth_token_expires_at_ms: Vec<u64>,
    pub conversation_handle_expires_at_ms: Vec<u64>,
    pub scope: &'static str,
}

pub(crate) fn snapshot(workspace_id: &str) -> SessionExpirySnapshot {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let current = registry()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(workspace_id)
        .cloned();
    let Some(current) = current else {
        return SessionExpirySnapshot {
            captured_at_ms: now,
            runtime_active: false,
            auth_type: "unknown".into(),
            oauth_token_ttl_seconds: None,
            oauth_token_expires_at_ms: vec![],
            conversation_handle_expires_at_ms: vec![],
            scope: "current_runtime",
        };
    };
    let mut handles = current
        .handles
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let expiries = handles.expiry_times(workspace_id, now);
    SessionExpirySnapshot {
        captured_at_ms: now,
        runtime_active: true,
        auth_type: current.auth_type.clone(),
        oauth_token_ttl_seconds: current
            .oauth
            .as_ref()
            .map(|oauth| oauth.token_ttl_seconds()),
        oauth_token_expires_at_ms: current
            .oauth
            .as_ref()
            .map_or_else(Vec::new, |oauth| oauth.token_expiry_times()),
        conversation_handle_expires_at_ms: expiries,
        scope: "current_runtime",
    }
}

/// Add the authenticated caller's expiry only after HTTP bearer verification.
pub(crate) fn decorate_caller_status(response: &mut Value, oauth_expiry: Option<u64>) {
    let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    let Some(structured) = result
        .get_mut("structuredContent")
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    if structured.get("ok").and_then(Value::as_bool) == Some(false) {
        return;
    }
    structured.insert("oauth_token_expires_at_ms".to_owned(), json!(oauth_expiry));
    let text = serde_json::to_string(structured).unwrap_or_default();
    if let Some(block) = result
        .get_mut("content")
        .and_then(Value::as_array_mut)
        .and_then(|items| {
            items
                .iter_mut()
                .find(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        })
    {
        block["text"] = json!(text);
    }
}

#[cfg(test)]
mod tests {
    use super::{decorate_caller_status, snapshot};
    use serde_json::json;

    #[test]
    fn caller_status_decorator_changes_only_expiration_metadata() {
        let mut response = json!({
            "jsonrpc": "2.0",
            "result": {
                "structuredContent": {"ok": true, "oauth_token_expires_at_ms": null},
                "content": [{"type": "text", "text": "old summary"}]
            }
        });
        decorate_caller_status(&mut response, Some(12345));
        assert_eq!(
            response["result"]["structuredContent"]["oauth_token_expires_at_ms"],
            12345
        );
        let text = response["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed, response["result"]["structuredContent"]);
    }

    #[test]
    fn inactive_runtime_has_no_fabricated_sessions() {
        let result = snapshot("missing-expiry-test-profile");
        assert!(!result.runtime_active);
        assert!(result.conversation_handle_expires_at_ms.is_empty());
        assert!(result.oauth_token_expires_at_ms.is_empty());
    }
}
