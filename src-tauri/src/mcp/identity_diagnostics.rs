//! Opt-in, bounded identity discovery. Never record raw header/metadata values.
use axum::http::HeaderMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::tools::redaction::redact_sensitive_text;

pub(crate) const ENABLE_ENV: &str = "CTMCP_MCP_IDENTITY_DIAGNOSTICS";
const MAX_KEYS: usize = 64;

fn label(value: &str) -> String {
    redact_sensitive_text(value)
        .0
        .chars()
        .filter(|ch| !ch.is_control())
        .take(128)
        .collect()
}

fn identity_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ![
        "auth",
        "cookie",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "api-key",
        "api_key",
        "apikey",
        "private",
    ]
    .iter()
    .any(|word| key.contains(word))
        && ["session", "conversation", "thread", "chat"]
            .iter()
            .any(|word| key.contains(word))
}

fn fingerprint(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 4096)
        .then(|| format!("{:x}", Sha256::digest(value.as_bytes())))
}

pub(crate) fn snapshot(headers: &HeaderMap, body: &Value) -> Value {
    let mut header_keys: Vec<_> = headers.keys().map(|key| key.as_str()).collect();
    header_keys.sort_unstable();
    let meta = body.pointer("/params/_meta").and_then(Value::as_object);
    let meta_keys: Vec<_> = meta.into_iter().flat_map(|meta| meta.keys()).collect();
    let mut candidates = Vec::new();
    for key in header_keys.iter().take(MAX_KEYS) {
        if identity_key(key) {
            if let Some(hash) = headers
                .get(*key)
                .and_then(|value| value.to_str().ok())
                .and_then(fingerprint)
            {
                candidates.push(json!({"source": "header", "key": label(key), "sha256": hash}));
            }
        }
    }
    for key in meta_keys.iter().take(MAX_KEYS) {
        if identity_key(key) {
            if let Some(hash) = meta
                .and_then(|meta| meta.get(*key))
                .and_then(Value::as_str)
                .and_then(fingerprint)
            {
                candidates.push(json!({"source": "meta", "key": label(key), "sha256": hash}));
            }
        }
    }
    let client_info = body
        .pointer("/params/clientInfo")
        .or_else(|| meta.and_then(|meta| meta.get("io.modelcontextprotocol/clientInfo")));
    json!({
        "method": body.get("method").and_then(Value::as_str).map(label),
        "tool": body.pointer("/params/name").and_then(Value::as_str).map(label),
        "header_keys": header_keys.iter().take(MAX_KEYS).map(|key| label(key)).collect::<Vec<_>>(),
        "meta_keys": meta_keys.iter().take(MAX_KEYS).map(|key| label(key)).collect::<Vec<_>>(),
        "keys_truncated": header_keys.len() > MAX_KEYS || meta_keys.len() > MAX_KEYS,
        "identity_candidates": candidates,
        "client_name": client_info.and_then(|info| info.get("name")).and_then(Value::as_str).map(label),
        "client_version": client_info.and_then(|info| info.get("version")).and_then(Value::as_str).map(label),
    })
}

pub(crate) fn record(profile_id: &str, headers: &HeaderMap, body: &Value) {
    if std::env::var(ENABLE_ENV).as_deref() != Ok("1") {
        return;
    }
    super::record_diagnostic_event(
        profile_id,
        "transport_event",
        "mcp_identity_observed",
        "info",
        "none",
        snapshot(headers, body),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_identity_without_recording_credentials_or_raw_values() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer credential-value".parse().unwrap());
        headers.insert("cookie", "private-cookie-value".parse().unwrap());
        headers.insert("mcp-session-id", "transport-identity".parse().unwrap());
        headers.insert("x-session-token", "excluded-token-value".parse().unwrap());
        let value = snapshot(
            &headers,
            &json!({"method": "tools/call", "params": {
                "name": "read_file", "arguments": {"path": "private-path"},
                "_meta": {"openai/session": "conversation-identity", "gemini/session_token": "excluded-meta-token"}
            }}),
        );
        assert_eq!(value["identity_candidates"].as_array().unwrap().len(), 2);
        let encoded = value.to_string();
        for secret in [
            "credential-value",
            "private-cookie-value",
            "transport-identity",
            "excluded-token-value",
            "conversation-identity",
            "excluded-meta-token",
            "private-path",
        ] {
            assert!(!encoded.contains(secret), "raw value leaked: {secret}");
        }
        assert_eq!(
            value["identity_candidates"][1]["sha256"],
            fingerprint("conversation-identity").unwrap()
        );
    }

    #[test]
    fn captures_client_info_and_bounds_untrusted_names_and_key_counts() {
        let meta = (0..80)
            .map(|index| (format!("field-{index}"), json!("ignored")))
            .collect::<serde_json::Map<_, _>>();
        let value = snapshot(
            &HeaderMap::new(),
            &json!({"method": "initialize", "params": {
                "clientInfo": {"name": "gemini-cli\n", "version": "1.2.3"}, "_meta": meta
            }}),
        );
        assert_eq!(value["client_name"], "gemini-cli");
        assert_eq!(value["client_version"], "1.2.3");
        assert_eq!(value["meta_keys"].as_array().unwrap().len(), 64);
        assert_eq!(value["keys_truncated"], true);
        assert!(
            snapshot(&HeaderMap::new(), &json!({}))["identity_candidates"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}
