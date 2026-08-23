use serde_json::{json, Value};

fn precise_edit_schema() -> Value {
    let text_target_properties = json!({
        "match_mode": {
            "type": "string",
            "enum": ["exact", "whitespace"],
            "default": "exact",
            "description": "exact requires identical content except CRLF/LF line endings; whitespace additionally tolerates whitespace differences. Inserted and replacement text is normalized to the target file's newline style."
        },
        "before_context": { "type": "string" },
        "after_context": { "type": "string" },
        "expected_occurrences": { "type": "integer", "minimum": 1, "default": 1 },
        "start_line": { "type": "integer", "minimum": 1 },
        "end_line": { "type": "integer", "minimum": 1 }
    });
    let text_target = text_target_properties
        .as_object()
        .expect("precise edit text target properties");

    let mut replace_properties = text_target.clone();
    replace_properties.insert(
        "type".into(),
        json!({ "type": "string", "enum": ["replace"] }),
    );
    replace_properties.insert(
        "old_text".into(),
        json!({ "type": "string", "minLength": 1 }),
    );
    replace_properties.insert("new_text".into(), json!({ "type": "string" }));

    let mut insert_before_properties = text_target.clone();
    insert_before_properties.insert(
        "type".into(),
        json!({ "type": "string", "enum": ["insert_before"] }),
    );
    insert_before_properties.insert("anchor".into(), json!({ "type": "string", "minLength": 1 }));
    insert_before_properties.insert("text".into(), json!({ "type": "string", "minLength": 1 }));

    let mut insert_after_properties = text_target.clone();
    insert_after_properties.insert(
        "type".into(),
        json!({ "type": "string", "enum": ["insert_after"] }),
    );
    insert_after_properties.insert("anchor".into(), json!({ "type": "string", "minLength": 1 }));
    insert_after_properties.insert("text".into(), json!({ "type": "string", "minLength": 1 }));

    json!({
        "oneOf": [
            {
                "type": "object",
                "properties": replace_properties,
                "required": ["type", "old_text", "new_text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": insert_before_properties,
                "required": ["type", "anchor", "text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": insert_after_properties,
                "required": ["type", "anchor", "text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "enum": ["replace_lines"] },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "end_line": { "type": "integer", "minimum": 1 },
                    "new_text": { "type": "string" },
                    "expected_text": { "type": "string" }
                },
                "required": ["type", "start_line", "end_line", "new_text"],
                "additionalProperties": false
            },
            {
                "type": "object",
                "properties": {
                    "type": { "type": "string", "enum": ["delete_lines"] },
                    "start_line": { "type": "integer", "minimum": 1 },
                    "end_line": { "type": "integer", "minimum": 1 },
                    "expected_text": { "type": "string" }
                },
                "required": ["type", "start_line", "end_line"],
                "additionalProperties": false
            }
        ]
    })
}

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let schema = match name {
        "apply_patch" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 },
                "dry_run": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "expected_sha256": {
                    "type": "object",
                    "additionalProperties": { "type": "string", "minLength": 64, "maxLength": 64 }
                },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        "edit" => json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "minLength": 1 },
                            "expected_sha256": { "type": "string", "minLength": 64, "maxLength": 64 },
                            "edits": {
                                "type": "array",
                                "minItems": 1,
                                "maxItems": 100,
                                "items": precise_edit_schema()
                            },
                            "apply_proposal": {
                                "type": "object",
                                "properties": {
                                    "proposal_id": { "type": "string", "minLength": 1 },
                                    "patch": {
                                        "type": "string",
                                        "maxLength": 65536,
                                        "description": "Optional economical unified diff limited to one file and one hunk, applied only to the proposal replacement text."
                                    },
                                    "replacement": {
                                        "type": "string",
                                        "maxLength": 131072,
                                        "description": "Optional complete final replacement for the proposal candidate region. Mutually exclusive with patch."
                                    }
                                },
                                "required": ["proposal_id"],
                                "additionalProperties": false
                            }
                        },
                        "required": ["path"],
                        "oneOf": [
                            { "required": ["edits"] },
                            { "required": ["apply_proposal"] }
                        ],
                        "additionalProperties": false
                    }
                },
                "dry_run": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["files"],
            "additionalProperties": false
        }),
        "format_files" => json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "maxItems": 10000,
                    "items": { "type": "string", "minLength": 1 },
                    "description": "Explicit workspace-relative files or directories. Optional for changed, staged, and project scopes."
                },
                "scope": {
                    "type": "string",
                    "enum": ["files", "changed", "staged", "project"],
                    "default": "files"
                },
                "mode": {
                    "type": "string",
                    "enum": ["plan", "check", "apply"],
                    "default": "plan"
                },
                "formatter": {
                    "type": "string",
                    "default": "auto",
                    "description": "auto or a registered formatter adapter ID"
                },
                "strict": { "type": "boolean", "default": false },
                "include_patterns": {
                    "type": "array",
                    "maxItems": 100,
                    "items": { "type": "string", "minLength": 1 }
                },
                "exclude_patterns": {
                    "type": "array",
                    "maxItems": 100,
                    "items": { "type": "string", "minLength": 1 }
                },
                "max_files": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 10000,
                    "default": 500
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 600000,
                    "default": 120000
                },
                "expected_sha256": {
                    "type": "object",
                    "maxProperties": 10000,
                    "additionalProperties": {
                        "type": "string",
                        "minLength": 64,
                        "maxLength": 64
                    }
                },
                "confirm": { "type": "boolean", "default": false },
                "max_diff_bytes": {
                    "type": "integer",
                    "minimum": 1024,
                    "maximum": 1048576,
                    "default": 262144
                },
                "reason": { "type": "string", "default": "" }
            },
            "additionalProperties": false
        }),
        "file_ops" => json!({
            "type": "object",
            "properties": {
                "operations": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": 100,
                    "items": {
                        "type": "object",
                        "properties": {
                            "type": { "type": "string", "enum": ["create", "delete", "copy", "move", "mkdir"] },
                            "path": { "type": "string", "minLength": 1 },
                            "destination": { "type": "string", "minLength": 1 },
                            "content": { "type": "string" },
                            "expected_sha256": { "type": "string", "minLength": 64, "maxLength": 64 },
                            "overwrite": { "type": "boolean", "default": false }
                        },
                        "required": ["type", "path"],
                        "additionalProperties": false
                    }
                },
                "dry_run": { "type": "boolean", "default": false },
                "confirm": { "type": "boolean", "default": false },
                "reason": { "type": "string", "default": "" }
            },
            "required": ["operations"],
            "additionalProperties": false
        }),
        "patch_check" => json!({
            "type": "object",
            "properties": {
                "patch": { "type": "string", "minLength": 1 }
            },
            "required": ["patch"],
            "additionalProperties": false
        }),
        _ => return None,
    };

    Some(schema)
}
