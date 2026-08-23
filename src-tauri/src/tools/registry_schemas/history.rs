use serde_json::{json, Value};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let schema = match name {
        "history_session_bootstrap" => json!({
            "type": "object",
            "properties": {
                "workspace_root": { "type": "string", "minLength": 1 },
                "session_key": { "type": "string", "minLength": 1 },
                "title": { "type": "string" },
                "history_dir": { "type": "string", "default": "docs/history-session" },
                "create_if_missing": { "type": "boolean", "default": true },
                "response_mode": { "type": "string", "enum": ["compact", "full"], "default": "compact" }
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
        _ => return None,
    };
    Some(schema)
}
