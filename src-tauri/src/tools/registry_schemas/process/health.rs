use serde_json::{json, Value};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let schema = match name {
        "exec_health_check" => json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        _ => return None,
    };
    Some(schema)
}
