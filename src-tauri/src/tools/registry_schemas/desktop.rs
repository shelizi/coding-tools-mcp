use serde_json::{json, Value};

pub(super) fn input_schema(name: &str) -> Option<Value> {
    let display_id = json!({ "type": "integer", "minimum": 0 });
    match name {
        "desktop_displays" => {
            Some(json!({"type":"object","properties":{},"additionalProperties":false}))
        }
        "desktop_screenshot" => Some(json!({
            "type":"object","properties":{
                "display_id": display_id,
                "quality":{"type":"integer","minimum":1,"maximum":100,"default":80},
                "output":{"type":"string","enum":["mcp_image","data_url"],"default":"mcp_image"}
            },"additionalProperties":false
        })),
        "desktop_click" => Some(json!({
            "type":"object","properties":{
                "display_id":display_id,"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},
                "button":{"type":"string","enum":["left","right","middle"],"default":"left"},
                "clicks":{"type":"integer","minimum":1,"maximum":3,"default":1}
            },"required":["x","y"],"additionalProperties":false
        })),
        "desktop_drag" => Some(json!({
            "type":"object","properties":{
                "display_id":display_id.clone(),"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},
                "to_display_id":display_id,"to_x":{"type":"integer","minimum":0},"to_y":{"type":"integer","minimum":0},
                "button":{"type":"string","enum":["left","right","middle"],"default":"left"},
                "duration_ms":{"type":"integer","minimum":0,"maximum":5000,"default":300},
                "steps":{"type":"integer","minimum":1,"maximum":120,"default":12}
            },"required":["x","y","to_x","to_y"],"additionalProperties":false
        })),
        "desktop_scroll" => Some(json!({
            "type":"object","properties":{
                "display_id":display_id,"x":{"type":"integer","minimum":0},"y":{"type":"integer","minimum":0},
                "delta_y":{"type":"integer","minimum":-12000,"maximum":12000},
                "delta_x":{"type":"integer","minimum":-12000,"maximum":12000,"default":0}
            },"required":["delta_y"],"additionalProperties":false
        })),
        "desktop_type" => Some(
            json!({"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}),
        ),
        "desktop_key" => Some(json!({
            "type":"object","properties":{"keys":{"type":"array","items":{"type":"string","minLength":1},"minItems":1,"maxItems":8}},
            "required":["keys"],"additionalProperties":false
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

        let click = input_schema("desktop_click").expect("click schema");
        assert_eq!(click["properties"]["clicks"]["minimum"], 1);
        assert_eq!(click["properties"]["clicks"]["maximum"], 3);
        assert_eq!(click["properties"]["clicks"]["default"], 1);

        let drag = input_schema("desktop_drag").expect("drag schema");
        assert_eq!(drag["properties"]["duration_ms"]["minimum"], 0);
        assert_eq!(drag["properties"]["duration_ms"]["maximum"], 5000);
        assert_eq!(drag["properties"]["duration_ms"]["default"], 300);
        assert_eq!(drag["properties"]["steps"]["minimum"], 1);
        assert_eq!(drag["properties"]["steps"]["maximum"], 120);
        assert_eq!(drag["properties"]["steps"]["default"], 12);

        let key = input_schema("desktop_key").expect("key schema");
        assert_eq!(key["properties"]["keys"]["type"], "array");
        assert_eq!(key["properties"]["keys"]["minItems"], 1);
        assert_eq!(key["properties"]["keys"]["maxItems"], 8);
        assert_eq!(key["required"][0], "keys");
    }
}
