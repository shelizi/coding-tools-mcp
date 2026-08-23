use serde_json::{json, Value};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    match name {
        "view_image" => Some(json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "minLength": 1 },
                "max_bytes": { "type": "integer", "minimum": 1024, "maximum": 10485760, "default": 5242880 },
                "max_width": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "max_height": { "type": "integer", "minimum": 1, "maximum": 10000, "default": 2000 },
                "auto_resize": { "type": "boolean", "default": true },
                "output": { "type": "string", "enum": ["mcp_image", "data_url"], "default": "mcp_image" }
            },
            "required": ["path"],
            "additionalProperties": false
        })),
        _ => None,
    }
}
