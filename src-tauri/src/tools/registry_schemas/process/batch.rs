use serde_json::{json, Value};

use crate::tools::ABSOLUTE_COMMAND_TIMEOUT_MAX_MS;

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let schema = match name {
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
                            "timeout_ms": { "type": "integer", "minimum": 1, "maximum": ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, "default": 30000 },
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
                "operation_id": { "type": "string", "minLength": 1, "maxLength": 128, "description": "Stable retained graph identifier. Reuse it without commands to reattach to the same exec_many graph instead of starting duplicate commands." },
                "action": { "type": "string", "enum": ["run", "status", "cancel", "forget"], "default": "run", "description": "Run or reattach by default; status returns immediately, cancel terminates active graph children, and forget releases a completed retained graph immediately." },
                "reason": { "type": "string", "default": "", "description": "Optional reason recorded when cancelling a retained graph." },
                "result_mode": { "type": "string", "enum": ["full", "summary", "none"], "description": "Controls per-command result detail. When omitted, run/reattach preserves full results while status/cancel use compact summaries to avoid repeating large stdout/stderr payloads." },
                "yield_time_ms": { "type": "integer", "minimum": 0, "maximum": 30000, "default": 30000, "description": "How long this exec_many call waits for graph completion before returning retained progress. The graph continues running after this window." },
                "mode": { "type": "string", "enum": ["auto", "sequential", "parallel", "dag"], "default": "auto", "description": "Execution scheduler. Auto uses dependencies, hard safety rules, resource locks, and historical pair statistics. Unknown command pairs remain sequential until explicit parallel observations provide enough safe evidence; explicit parallel is never silently overridden." },
                "max_parallel": { "type": "integer", "minimum": 1, "maximum": 256, "description": "Maximum concurrently running batch commands. Auto mode bounds the requested value by 8, workspace process admission, and historical resource-serialization recommendations." },
                "stop_on_error": { "type": "boolean", "default": true }
            },
            "additionalProperties": false
        }),
        _ => return None,
    };
    Some(schema)
}
