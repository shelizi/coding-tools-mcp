use serde_json::{json, Value};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let display_id = json!({ "type": "integer", "minimum": 0 });
    let coordinate_reference = json!({
        "type": "object",
        "description": "Pass desktop_screenshot.coordinate_reference unchanged. When present x/y use JPEG image pixels and the current display bounds must still match; otherwise x/y are physical display pixels.",
        "properties": {
            "id": {"type":"integer","minimum":0}, "name": {"type":"string","minLength":1},
            "x": {"type":"integer"}, "y": {"type":"integer"},
            "width": {"type":"integer","minimum":1}, "height": {"type":"integer","minimum":1},
            "image_width": {"type":"integer","minimum":1}, "image_height": {"type":"integer","minimum":1}
        },
        "required":["id","name","x","y","width","height","image_width","image_height"],
        "additionalProperties":false
    });
    match name {
        "desktop_displays" => {
            Some(json!({"type":"object","properties":{},"additionalProperties":false}))
        }
        "desktop_screenshot" => Some(json!({
            "type":"object","properties":{
                "display_id": display_id,
                "quality":{"type":"integer","minimum":1,"maximum":100,"default":80},
                "max_width":{"type":"integer","minimum":1,"maximum":16384,"description":"Optional JPEG width cap, preserving aspect ratio and never enlarging."},
                "max_height":{"type":"integer","minimum":1,"maximum":16384,"description":"Optional JPEG height cap, preserving aspect ratio and never enlarging."},
                "output":{"type":"string","enum":["mcp_image","data_url"],"default":"mcp_image"}
            },"additionalProperties":false
        })),
        "desktop_input" => Some(json!({
            "type":"object",
            "description":"Execute all actions in order, in one input session. Use key_down then click/drag/move and key_up for multi-selection; modifiers on click/drag/scroll are temporarily held. All pressed inputs are released in finally, including on error. Coordinates are physical pixels unless paired with desktop_screenshot.coordinate_reference. For cross-display image drag provide the target reference or per-waypoint reference.",
            "properties":{
                "capture_after":{"type":"boolean","default":false,"description":"Return a fresh screenshot in this same MCP result after actions complete, avoiding a second MCP call."},
                "capture_display_id":{"type":"integer","minimum":0,"description":"Display to capture after input; defaults to primary."},
                "capture_max_width":{"type":"integer","minimum":1,"maximum":16384,"default":1280},
                "capture_quality":{"type":"integer","minimum":1,"maximum":100,"default":75},
                "actions":{
                    "type":"array","minItems":1,"maxItems":64,
                    "description":"A single atomic-like validated sequence. All actions validated before any OS input is sent. Max total wait/drag duration is 20000ms. On bridge errors held keys/buttons are released.",
                    "items":{
                        "type":"object",
                        "properties":{
                            "type":{"type":"string","enum":["click","drag","scroll","type","key","key_down","key_up","mouse_down","mouse_up","move","wait"]},
                            "display_id":display_id.clone(),
                            "x":{"type":"integer","minimum":0},
                            "y":{"type":"integer","minimum":0},
                            "coordinate_reference":coordinate_reference.clone(),
                            "to_display_id":display_id,
                            "to_x":{"type":"integer","minimum":0},
                            "to_y":{"type":"integer","minimum":0},
                            "to_coordinate_reference":coordinate_reference.clone(),
                            "path":{"type":"array","minItems":1,"maxItems":32,
                                "description":"Optional drag waypoints; do not combine with to_x/to_y. Each waypoint can target another display with its own screenshot reference.",
                                "items":{"type":"object","properties":{
                                    "display_id":{"type":"integer","minimum":0},
                                    "x":{"type":"integer","minimum":0},
                                    "y":{"type":"integer","minimum":0},
                                    "coordinate_reference":coordinate_reference
                                },"required":["x","y"],"additionalProperties":false}
                            },
                            "button":{"type":"string","enum":["left","right","middle"],"default":"left"},
                            "clicks":{"type":"integer","minimum":1,"maximum":3,"default":1},
                            "modifiers":{"type":"array","description":"Temporarily hold modifier keys during click, drag or scroll, e.g. [\"CTRL\",\"SHIFT\"].","items":{"type":"string","minLength":1},"minItems":1,"maxItems":8},
                            "keys":{"type":"array","description":"Keys for key, key_down or key_up. A key chord presses all keys then releases them in reverse order.","items":{"type":"string","minLength":1},"minItems":1,"maxItems":8},
                            "text":{"type":"string","maxLength":16384},
                            "delta_y":{"type":"integer","minimum":-12000,"maximum":12000},
                            "delta_x":{"type":"integer","minimum":-12000,"maximum":12000},
                            "duration_ms":{"type":"integer","minimum":0,"maximum":5000},
                            "steps":{"type":"integer","minimum":1,"maximum":120}
                        },
                        "required":["type"],
                        "additionalProperties":false
                    }
                }
            },
            "required":["actions"],
            "additionalProperties":false
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::input_schema;

    #[test]
    fn desktop_contract_boundaries_are_stable() {
        let screenshot = input_schema("desktop_screenshot").expect("screenshot schema");
        assert_eq!(screenshot["properties"]["quality"]["minimum"], 1);
        assert_eq!(screenshot["properties"]["quality"]["maximum"], 100);
        assert_eq!(screenshot["properties"]["quality"]["default"], 80);
        assert_eq!(screenshot["properties"]["max_width"]["maximum"], 16384);
        assert_eq!(screenshot["properties"]["max_height"]["maximum"], 16384);

        let input = input_schema("desktop_input").expect("unified input schema");
        assert_eq!(input["properties"]["actions"]["minItems"], 1);
        assert_eq!(input["properties"]["actions"]["maxItems"], 64);
        assert_eq!(
            input["properties"]["actions"]["items"]["properties"]["coordinate_reference"]
                ["required"]
                .as_array()
                .unwrap()
                .len(),
            8
        );
        let actions = &input["properties"]["actions"]["items"]["properties"];
        assert_eq!(actions["duration_ms"]["maximum"], 5000);
        assert_eq!(actions["steps"]["maximum"], 120);
        assert_eq!(
            actions["to_coordinate_reference"]["properties"]["image_width"]["minimum"],
            1
        );
        assert_eq!(actions["keys"]["type"], "array");
        assert_eq!(actions["keys"]["maxItems"], 8);
        assert_eq!(actions["path"]["maxItems"], 32);
        assert_eq!(input["required"][0], "actions");
    }
}
