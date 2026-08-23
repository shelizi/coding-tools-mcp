use serde_json::{json, Value};

use crate::tools::ABSOLUTE_COMMAND_TIMEOUT_MAX_MS;

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let schema = match name {
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
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, "default": 30000 },
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
                                "timeout_ms": { "type": "integer", "minimum": 1, "maximum": ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, "default": 30000 },
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
        _ => return None,
    };
    Some(schema)
}
