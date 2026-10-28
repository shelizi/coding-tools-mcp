use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use super::{
    display_json, key_code, map_display_point_with_reference, mouse_button_flags, run_ps, tool_ok,
    type_payload, WorkspaceError,
};

fn invalid(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::invalid_argument(message)
}

fn integer(
    action: &Value,
    key: &str,
    min: i64,
    max: i64,
    default: Option<i64>,
) -> Result<i64, WorkspaceError> {
    let n = match action.get(key) {
        None => default.ok_or_else(|| invalid(format!("{key} is required")))?,
        Some(v) => v
            .as_i64()
            .ok_or_else(|| invalid(format!("{key} must be an integer")))?,
    };
    if n < min || n > max {
        return Err(invalid(format!("{key} must be between {min} and {max}")));
    }
    Ok(n)
}

fn codes(action: &Value, key: &str) -> Result<Vec<u16>, WorkspaceError> {
    let values = action
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{key} must contain 1 to 8 key names")))?;
    if values.is_empty() || values.len() > 8 {
        return Err(invalid(format!("{key} must contain 1 to 8 key names")));
    }
    let mut unique = HashSet::new();
    values
        .iter()
        .map(|value| {
            let name = value
                .as_str()
                .ok_or_else(|| invalid(format!("{key} must contain strings")))?;
            let code = key_code(name).ok_or_else(|| invalid(format!("unsupported key: {name}")))?;
            if !unique.insert(code) {
                return Err(invalid(format!("{key} cannot contain repeated keys")));
            }
            Ok(code)
        })
        .collect()
}

fn button(action: &Value) -> Result<(u32, u32), WorkspaceError> {
    let name = action
        .get("button")
        .and_then(Value::as_str)
        .unwrap_or("left");
    mouse_button_flags(name)
}

struct Plan<F: FnMut(Option<u64>) -> Result<Value, WorkspaceError>> {
    resolve: F,
    screens: HashMap<Option<u64>, Value>,
    used: HashMap<u64, Value>,
    keys: HashSet<u16>,
    buttons: HashSet<u32>,
    instructions: Vec<String>,
    duration: i64,
}

impl<F: FnMut(Option<u64>) -> Result<Value, WorkspaceError>> Plan<F> {
    fn point(
        &mut self,
        action: &Value,
        target: bool,
        fallback_id: Option<u64>,
        fallback_reference: Option<&Value>,
    ) -> Result<(i32, i32, u64), WorkspaceError> {
        let id_key = if target {
            "to_display_id"
        } else {
            "display_id"
        };
        let id = action
            .get(id_key)
            .map(|_| integer(action, id_key, 0, 64, None).map(|value| value as u64))
            .transpose()?
            .or(fallback_id);
        let display = if let Some(d) = self.screens.get(&id) {
            d.clone()
        } else {
            let d = (self.resolve)(id)?;
            self.screens.insert(id, d.clone());
            d
        };
        let x = integer(
            action,
            if target { "to_x" } else { "x" },
            0,
            1_000_000,
            None,
        )? as i32;
        let y = integer(
            action,
            if target { "to_y" } else { "y" },
            0,
            1_000_000,
            None,
        )? as i32;
        let reference = action
            .get(if target {
                "to_coordinate_reference"
            } else {
                "coordinate_reference"
            })
            .or(fallback_reference);
        let (gx, gy) = map_display_point_with_reference(&display, x, y, reference)?;
        let screen_id = display["id"]
            .as_u64()
            .ok_or_else(|| invalid("invalid display id"))?;
        if display["name"].as_str().is_none() {
            return Err(invalid("invalid display name"));
        }
        self.used.insert(screen_id, display);
        Ok((gx, gy, screen_id))
    }

    fn scoped(&mut self, action: &Value, body: Vec<String>) -> Result<(), WorkspaceError> {
        let modifiers = match action.get("modifiers") {
            Some(_) => codes(&json!({"keys": action["modifiers"]}), "keys")?,
            None => Vec::new(),
        };
        let introduced = modifiers
            .into_iter()
            .filter(|k| !self.keys.contains(k))
            .collect::<Vec<_>>();
        for code in &introduced {
            self.instructions.push(down_key(*code));
        }
        self.instructions.extend(body);
        for code in introduced.iter().rev() {
            self.instructions.push(up_key(*code));
        }
        Ok(())
    }
}

fn down_key(code: u16) -> String {
    format!("[ComputerUseNative]::KeyDown({code});$heldKeys.Add({code})")
}
fn up_key(code: u16) -> String {
    format!("[ComputerUseNative]::KeyUp({code});[void]$heldKeys.Remove({code})")
}
fn down_mouse(down: u32, up: u32) -> String {
    format!("[ComputerUseNative]::MouseDown({down});$heldButtons.Add({up})")
}
fn up_mouse(up: u32) -> String {
    format!("[ComputerUseNative]::MouseUp({up});[void]$heldButtons.Remove({up})")
}
fn move_to((x, y, _): (i32, i32, u64)) -> String {
    format!("[ComputerUseNative]::Move({x},{y})")
}

struct PlannedInput {
    script: String,
    action_count: usize,
    display_count: usize,
}

fn plan_input<F: FnMut(Option<u64>) -> Result<Value, WorkspaceError>>(
    args: &Value,
    resolve: F,
) -> Result<PlannedInput, WorkspaceError> {
    let actions = args
        .get("actions")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("actions must contain 1 to 64 operations"))?;
    if actions.is_empty() || actions.len() > 64 {
        return Err(invalid("actions must contain 1 to 64 operations"));
    }
    let mut plan = Plan {
        resolve,
        screens: HashMap::new(),
        used: HashMap::new(),
        keys: HashSet::new(),
        buttons: HashSet::new(),
        instructions: Vec::new(),
        duration: 0,
    };
    for (index, action) in actions.iter().enumerate() {
        let kind = action
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("actions[{index}].type is required")))?;
        match kind {
            "key_down" => {
                for code in codes(action, "keys")? {
                    if !plan.keys.insert(code) {
                        return Err(invalid("key_down cannot hold a key twice"));
                    }
                    plan.instructions.push(down_key(code));
                }
            }
            "key_up" => {
                for code in codes(action, "keys")?.iter().rev() {
                    if !plan.keys.remove(code) {
                        return Err(invalid("key_up without a matching key_down"));
                    }
                    plan.instructions.push(up_key(*code));
                }
            }
            "key" => {
                let codes = codes(action, "keys")?;
                if codes.iter().any(|c| plan.keys.contains(c)) {
                    return Err(invalid(
                        "key shortcut overlaps a held key; use key_down and key_up",
                    ));
                }
                plan.instructions.push(format!(
                    "[ComputerUseNative]::Hotkey([uint16[]]@({}))",
                    codes
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(",")
                ));
            }
            "type" => {
                let text = action
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("text must be a string up to 16384 characters"))?;
                if text.encode_utf16().count() > 16384 {
                    return Err(invalid("text must be a string up to 16384 characters"));
                }
                let (encoded, _) = type_payload(text);
                plan.instructions.push(format!("[ComputerUseNative]::TypeUnicode([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')))"));
            }
            "move" => {
                let p = plan.point(action, false, None, None)?;
                plan.instructions.push(move_to(p));
            }
            "mouse_down" | "mouse_up" => {
                let (down, up) = button(action)?;
                if kind == "mouse_down" {
                    if !plan.buttons.insert(up) {
                        return Err(invalid("mouse_down cannot hold a button twice"));
                    }
                } else if !plan.buttons.remove(&up) {
                    return Err(invalid("mouse_up without matching mouse_down"));
                }
                if action.get("x").is_some() || action.get("y").is_some() {
                    let p = plan.point(action, false, None, None)?;
                    plan.instructions.push(move_to(p));
                }
                plan.instructions.push(if kind == "mouse_down" {
                    down_mouse(down, up)
                } else {
                    up_mouse(up)
                });
            }
            "click" => {
                let (down, up) = button(action)?;
                if plan.buttons.contains(&up) {
                    return Err(invalid("click cannot reuse a held mouse button"));
                }
                let p = plan.point(action, false, None, None)?;
                let clicks = integer(action, "clicks", 1, 3, Some(1))?;
                let mut body = vec![move_to(p)];
                for _ in 0..clicks {
                    body.push(down_mouse(down, up));
                    body.push(up_mouse(up));
                }
                plan.scoped(action, body)?;
            }
            "drag" => {
                let (down, up) = button(action)?;
                if plan.buttons.contains(&up) {
                    return Err(invalid("drag cannot reuse a held mouse button"));
                }
                let start = plan.point(action, false, None, None)?;
                let mut pts = vec![start];
                if let Some(path) = action.get("path") {
                    let path = path
                        .as_array()
                        .ok_or_else(|| invalid("drag.path must contain 1 to 32 waypoints"))?;
                    if path.is_empty()
                        || path.len() > 32
                        || action.get("to_x").is_some()
                        || action.get("to_y").is_some()
                    {
                        return Err(invalid("drag.path must contain 1 to 32 waypoints and cannot be combined with to_x/to_y"));
                    }
                    for waypoint in path {
                        let id = waypoint
                            .get("display_id")
                            .map(|_| integer(waypoint, "display_id", 0, 64, None))
                            .transpose()?;
                        let reference = waypoint.get("coordinate_reference").or_else(|| {
                            if id.is_none() || id == Some(start.2 as i64) {
                                action.get("coordinate_reference")
                            } else {
                                None
                            }
                        });
                        if action.get("coordinate_reference").is_some()
                            && id.is_some()
                            && id != Some(start.2 as i64)
                            && waypoint.get("coordinate_reference").is_none()
                        {
                            return Err(invalid(
                                "cross-display waypoints require coordinate_reference",
                            ));
                        }
                        pts.push(plan.point(waypoint, false, Some(start.2), reference)?);
                    }
                } else {
                    let end_id =
                        integer(action, "to_display_id", 0, 64, Some(start.2 as i64))? as u64;
                    if action.get("coordinate_reference").is_some()
                        && end_id != start.2
                        && action.get("to_coordinate_reference").is_none()
                    {
                        return Err(invalid(
                            "cross-display drag requires to_coordinate_reference",
                        ));
                    }
                    let reference = if end_id == start.2 {
                        action.get("coordinate_reference")
                    } else {
                        None
                    };
                    pts.push(plan.point(action, true, Some(start.2), reference)?);
                }
                let duration = integer(action, "duration_ms", 0, 5000, Some(300))?;
                let steps = integer(action, "steps", 1, 120, Some(12))?;
                plan.duration += duration;
                let coords = pts
                    .iter()
                    .flat_map(|(x, y, _)| [x.to_string(), y.to_string()])
                    .collect::<Vec<_>>()
                    .join(",");
                plan.scoped(
                    action,
                    vec![
                        move_to(start),
                        down_mouse(down, up),
                        format!(
                            "[ComputerUseNative]::MovePath([int[]]@({coords}),{duration},{steps})"
                        ),
                        up_mouse(up),
                    ],
                )?;
            }
            "scroll" => {
                let dx = integer(action, "delta_x", -12000, 12000, Some(0))?;
                let dy = integer(action, "delta_y", -12000, 12000, Some(0))?;
                if dx == 0 && dy == 0 {
                    return Err(invalid("scroll requires nonzero delta_x or delta_y"));
                }
                let mut body = Vec::new();
                if action.get("x").is_some() || action.get("y").is_some() {
                    let p = plan.point(action, false, None, None)?;
                    body.push(move_to(p));
                }
                if dy != 0 {
                    body.push(format!("[ComputerUseNative]::Wheel(2048,{dy})"));
                }
                if dx != 0 {
                    body.push(format!("[ComputerUseNative]::Wheel(4096,{dx})"));
                }
                plan.scoped(action, body)?;
            }
            "wait" => {
                let duration = integer(action, "duration_ms", 0, 2000, None)?;
                plan.duration += duration;
                plan.instructions
                    .push(format!("[System.Threading.Thread]::Sleep({duration})"));
            }
            _ => return Err(invalid(format!("unsupported desktop action: {kind}"))),
        }
        if plan.duration > 20000 {
            return Err(invalid("desktop input duration exceeds 20000 ms"));
        }
    }
    let mut checks = Vec::new();
    let mut used = plan.used.into_iter().collect::<Vec<_>>();
    used.sort_by_key(|(id, _)| *id);
    for (id, display) in &used {
        let name = STANDARD.encode(display["name"].as_str().unwrap_or_default());
        checks.push(format!(
            "if($all.Count -le {id}){{throw 'Display changed since snapshot'}}"
        ));
        checks.push(format!(
            "$s=$all[{id}];if($s.DeviceName -ne [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{name}')) -or $s.Bounds.X -ne {x} -or $s.Bounds.Y -ne {y} -or $s.Bounds.Width -ne {w} -or $s.Bounds.Height -ne {h}){{throw 'Display changed since snapshot'}}",
            x=display["x"], y=display["y"], w=display["width"], h=display["height"]
        ));
    }
    let mut lines = vec![
        "$heldKeys=[System.Collections.Generic.List[int]]::new()".into(),
        "$heldButtons=[System.Collections.Generic.List[int]]::new()".into(),
        "$desktopMutex=[System.Threading.Mutex]::new($false,'Local\\CodingToolsMCPDesktopInput_v1')".into(),
        "$locked=$false".into(),
        "try {".into(),
        "try{$locked=$desktopMutex.WaitOne(10000)}catch [System.Threading.AbandonedMutexException]{$locked=$true}".into(),
        "if(-not $locked){throw 'Desktop input busy'}".into(),
        "try {".into(),
    ];
    if !checks.is_empty() {
        lines.push("$all=@([System.Windows.Forms.Screen]::AllScreens)".into());
    }
    lines.extend(checks);
    lines.extend(plan.instructions);
    lines.extend([
        "} finally {".into(),
        "for($i=$heldButtons.Count-1;$i -ge 0;$i--){try{[ComputerUseNative]::MouseUp($heldButtons[$i])}catch{}}".into(),
        "for($i=$heldKeys.Count-1;$i -ge 0;$i--){try{[ComputerUseNative]::KeyUp($heldKeys[$i])}catch{}}".into(),
        "}".into(),
        "} finally {if($locked){[void]$desktopMutex.ReleaseMutex()};$desktopMutex.Dispose()}".into(),
    ]);
    Ok(PlannedInput {
        script: lines.join("\n"),
        action_count: actions.len(),
        display_count: used.len(),
    })
}

pub fn input(args: &Value) -> Result<Value, WorkspaceError> {
    let plan = plan_input(args, display_json)?;
    run_ps(&plan.script)?;
    let mut result = json!({
        "action_count": plan.action_count,
        "display_count": plan.display_count,
        "all_inputs_released": true
    });
    if args
        .get("capture_after")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let mut image_args = json!({
            "max_width": args.get("capture_max_width").and_then(Value::as_u64).unwrap_or(1280),
            "quality": args.get("capture_quality").and_then(Value::as_u64).unwrap_or(75)
        });
        if let Some(id) = args.get("capture_display_id").and_then(Value::as_u64) {
            image_args["display_id"] = json!(id);
        }
        match super::screenshot(&image_args) {
            Ok(image) => result["screenshot"] = image,
            Err(error) => result["screenshot_error"] = json!(error.message()),
        }
    }
    Ok(tool_ok(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: Option<u64>) -> Result<Value, WorkspaceError> {
        Ok(match id.unwrap_or(0) {
            0 => json!({"id":0,"name":"DISPLAY1","x":0,"y":0,"width":3840,"height":2160}),
            1 => json!({"id":1,"name":"DISPLAY2","x":-2560,"y":180,"width":2560,"height":1440}),
            _ => return Err(invalid("unknown screen")),
        })
    }

    #[test]
    fn input_batch_supports_multikey_mouse_drag_waypoints_and_auto_release() {
        let ref0 = json!({"id":0,"name":"DISPLAY1","x":0,"y":0,"width":3840,"height":2160,"image_width":1920,"image_height":1080});
        let ref1 = json!({"id":1,"name":"DISPLAY2","x":-2560,"y":180,"width":2560,"height":1440,"image_width":1280,"image_height":720});
        let request = json!({"actions":[
            {"type":"key_down","keys":["CTRL","SHIFT"]},
            {"type":"click","display_id":0,"x":500,"y":300,"coordinate_reference":ref0},
            {"type":"drag","display_id":0,"x":500,"y":300,"coordinate_reference":ref0,
             "path":[{"display_id":1,"x":1279,"y":719,"coordinate_reference":ref1}],
             "modifiers":["ALT"],"steps":12},
            {"type":"key_up","keys":["CTRL","SHIFT"]},
            {"type":"key","keys":["ALT","F4"]},
            {"type":"type","text":"中文😀"},
            {"type":"scroll","delta_y":-120,"x":0,"y":0}
        ]});
        let plan = plan_input(&request, display).unwrap();
        assert_eq!(plan.action_count, 7);
        assert_eq!(plan.display_count, 2);
        assert!(plan.script.contains("[ComputerUseNative]::KeyDown(17)"));
        assert!(plan.script.contains("[ComputerUseNative]::KeyDown(16)"));
        assert!(plan.script.contains("[ComputerUseNative]::Move(1000,600)"));
        assert!(plan
            .script
            .contains("[ComputerUseNative]::MovePath([int[]]@(1000,600,-1,1619),300,12)"));
        assert!(plan
            .script
            .contains("[ComputerUseNative]::MouseUp($heldButtons[$i])"));
        assert!(plan.script.contains("CodingToolsMCPDesktopInput_v1"));
        assert!(plan
            .script
            .contains("TypeUnicode([Text.Encoding]::UTF8.GetString"));
    }

    #[test]
    fn input_batch_rejects_invalid_sequences_before_injecting_any_events() {
        for request in [
            json!({"actions":[{"type":"key_up","keys":["CTRL"]}]}),
            json!({"actions":[{"type":"mouse_up","button":"left"}]}),
            json!({"actions":[{"type":"key_down","keys":["CTRL"]},{"type":"key_down","keys":["CTRL"]}]}),
            json!({"actions":[{"type":"drag","x":1,"y":1,"to_x":5,"to_y":5,"steps":0}]}),
            json!({"actions":[{"type":"click","x":2000,"y":0,"coordinate_reference":
                {"id":0,"name":"DISPLAY1","x":0,"y":0,"width":3840,"height":2160,"image_width":1920,"image_height":1080}}]}),
            json!({"actions":[{"type":"nonsense"}]}),
        ] {
            assert!(plan_input(&request, display).is_err(), "{request}");
        }
    }

    #[test]
    fn rust_input_bridge_wait_only_smoke_is_explicitly_opt_in() {
        if std::env::var("CTMCP_DESKTOP_INPUT_RUST_SMOKE").as_deref() != Ok("1") {
            return;
        }
        // No mouse or keyboard events: validates the compiled Rust PowerShell
        // preamble, Windows C# bridge, OS mutex, and finally cleanup only.
        let result = input(&json!({"actions":[{"type":"wait","duration_ms":0}]}))
            .expect("wait-only input bridge should start and exit cleanly");
        assert_eq!(result["ok"], true);
    }
}
