use serde_json::{json, Value};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    match name {
        "request_permissions" => Some(json!({
            "type": "object",
            "properties": {
                "resume_id": { "type": "string", "minLength": 1 },
                "approve": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "tool_name": {
                    "type": "string",
                    "enum": ["exec_command", "apply_patch", "git_push"]
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
        })),
        _ => None,
    }
}
