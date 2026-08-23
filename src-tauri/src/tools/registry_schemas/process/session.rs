use serde_json::{json, Value};

use crate::tools::session::{WAIT_COMMAND_TIMEOUT_DEFAULT_MS, WAIT_COMMAND_TIMEOUT_MAX_MS};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let schema = match name {
        "wait_command" => json!({
            "type": "object",
            "properties": {
                "session_id": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 0, "default": 0 },
                "timeout_ms": { "type": "integer", "minimum": 0, "maximum": WAIT_COMMAND_TIMEOUT_MAX_MS, "default": WAIT_COMMAND_TIMEOUT_DEFAULT_MS, "description": "Server-side event wait, separate from the child-process timeout. The MCP transport sends a heartbeat every 10 seconds to keep long requests alive. Use output_or_exit for live incremental status; the wait window may be up to 60 minutes." },
                "heartbeat_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 0, "description": "Deprecated compatibility field. Accepted but ignored for application wait timing; MCP transport heartbeats keep long requests alive automatically." },
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
        _ => return None,
    };
    Some(schema)
}
