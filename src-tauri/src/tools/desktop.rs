use std::process::Command;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};

use crate::tools::workspace::{tool_ok, WorkspaceError};

#[cfg(target_os = "windows")]
mod batch;
#[cfg(target_os = "windows")]
pub use batch::input;

#[cfg(not(target_os = "windows"))]
pub fn input(_args: &Value) -> Result<Value, WorkspaceError> {
    Err(unsupported())
}

const PS_PREAMBLE: &str = r#"
$ErrorActionPreference='Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
if (-not ('ComputerUseNative' -as [type])) {
Add-Type @'
using System;
using System.Runtime.InteropServices;
using System.Threading;
public static class ComputerUseNative {
  [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr value);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll", SetLastError=true)] public static extern bool SetPhysicalCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public HARDWAREINPUT hi; }
  [DllImport("user32.dll", SetLastError=true)] public static extern uint SendInput(uint nInputs, INPUT[] inputs, int cbSize);
  static int InputSize() {
    int size = Marshal.SizeOf(typeof(INPUT));
    int expected = IntPtr.Size == 8 ? 40 : 28;
    if (size != expected) throw new InvalidOperationException("Unexpected INPUT size: " + size + " (expected " + expected + ")");
    return size;
  }
  static void Send(INPUT[] inputs) {
    uint sent = SendInput((uint)inputs.Length, inputs, InputSize());
    if (sent != inputs.Length) throw new InvalidOperationException("SendInput failed with Win32 error " + Marshal.GetLastWin32Error());
  }
  public static void Move(int x, int y) {
    if (!SetPhysicalCursorPos(x, y)) throw new InvalidOperationException("SetPhysicalCursorPos failed with Win32 error " + Marshal.GetLastWin32Error());
  }
  public static void KeyDown(ushort key) {
    Send(new INPUT[] { new INPUT { type=1, U=new InputUnion { ki=new KEYBDINPUT { wVk=key } } } });
  }
  public static void KeyUp(ushort key) {
    Send(new INPUT[] { new INPUT { type=1, U=new InputUnion { ki=new KEYBDINPUT { wVk=key, dwFlags=2 } } } });
  }
  public static void MouseDown(uint flag) { mouse_event(flag,0,0,0,UIntPtr.Zero); }
  public static void MouseUp(uint flag) { mouse_event(flag,0,0,0,UIntPtr.Zero); }
  public static void MovePath(int[] points,int durationMs,int steps) {
    int segments=points.Length/2-1;
    int total=Math.Max(steps,segments);
    for(int i=1;i<=total;i++) {
      double progress=(double)i*segments/total;
      int segment=Math.Min(segments-1,(int)Math.Floor(progress));
      double part=progress-segment;
      int j=2*segment;
      Move(points[j]+(int)Math.Round((points[j+2]-points[j])*part),
           points[j+1]+(int)Math.Round((points[j+3]-points[j+1])*part));
      if(durationMs>0) Thread.Sleep(durationMs/total);
    }
  }
  public static void TypeUnicode(string text) {
    foreach (char ch in text) {
      var down = new INPUT { type=1, U=new InputUnion { ki=new KEYBDINPUT { wScan=ch, dwFlags=4 } } };
      var up = new INPUT { type=1, U=new InputUnion { ki=new KEYBDINPUT { wScan=ch, dwFlags=6 } } };
      Send(new INPUT[] { down, up });
    }
  }
  public static void Hotkey(ushort[] keys) {
    var inputs = new INPUT[keys.Length * 2]; int i=0;
    foreach (ushort key in keys) inputs[i++] = new INPUT { type=1, U=new InputUnion { ki=new KEYBDINPUT { wVk=key } } };
    for (int k=keys.Length-1;k>=0;k--) inputs[i++] = new INPUT { type=1, U=new InputUnion { ki=new KEYBDINPUT { wVk=keys[k], dwFlags=2 } } };
    Send(inputs);
  }
  public static void Wheel(uint flag, int delta) {
    mouse_event(flag, 0, 0, unchecked((uint)delta), UIntPtr.Zero);
  }
  public static void Drag(int fromX, int fromY, int toX, int toY, uint down, uint up, int durationMs, int steps) {
    Move(fromX, fromY);
    mouse_event(down, 0, 0, 0, UIntPtr.Zero);
    try {
      int delay = steps > 0 ? durationMs / steps : 0;
      for (int i=1; i<=steps; i++) {
        double t = i / (double)steps;
        int x = fromX + (int)Math.Round((toX - fromX) * t);
        int y = fromY + (int)Math.Round((toY - fromY) * t);
        Move(x, y);
        if (delay > 0) Thread.Sleep(delay);
      }
    } finally {
      mouse_event(up, 0, 0, 0, UIntPtr.Zero);
    }
  }
}
'@
}
[void][ComputerUseNative]::SetThreadDpiAwarenessContext([IntPtr](-4))
"#;

fn runtime_error(message: impl Into<String>) -> WorkspaceError {
    WorkspaceError::Tool {
        code: "DESKTOP_ERROR",
        message: message.into(),
        category: "runtime",
        retryable: false,
    }
}

fn unsupported() -> WorkspaceError {
    WorkspaceError::Tool {
        code: "UNSUPPORTED_PLATFORM",
        message: "Desktop computer-use tools are currently supported on Windows only.".into(),
        category: "runtime",
        retryable: false,
    }
}

#[cfg(target_os = "windows")]
fn run_ps(body: &str) -> Result<String, WorkspaceError> {
    use std::os::windows::process::CommandExt;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    // PowerShell -Command - reads stdin line-by-line: the operation body
    // must be a single complete statement for try/finally to execute.
    let script = format!(
        "{PS_PREAMBLE}\n{}\n",
        body.lines().collect::<Vec<_>>().join(";")
    );
    let mut command = Command::new("powershell.exe");
    command.creation_flags(CREATE_NO_WINDOW);
    let output = command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            "-",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child
                .stdin
                .as_mut()
                .ok_or_else(|| std::io::Error::other("missing stdin"))?
                .write_all(script.as_bytes())?;
            child.wait_with_output()
        })
        .map_err(|e| runtime_error(format!("Failed to start desktop bridge: {e}")))?;
    if !output.status.success() {
        return Err(runtime_error(format!(
            "Desktop bridge failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "windows")]
fn display_json(display_id: Option<u64>) -> Result<Value, WorkspaceError> {
    let requested = display_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "$null".into());
    let body = format!(
        r#"
$all=@([System.Windows.Forms.Screen]::AllScreens)
$items=@()
for($i=0;$i -lt $all.Count;$i++){{ $s=$all[$i]; $items += [pscustomobject]@{{ id=$i; name=$s.DeviceName; x=$s.Bounds.X; y=$s.Bounds.Y; width=$s.Bounds.Width; height=$s.Bounds.Height; primary=$s.Primary }} }}
$id={requested}
if($null -eq $id){{ $selected=$items | Where-Object primary | Select-Object -First 1; if($null -eq $selected){{$selected=$items[0]}} }} else {{ $selected=$items | Where-Object id -eq $id | Select-Object -First 1 }}
if($null -eq $selected){{ throw 'Unknown display_id' }}
$selected | ConvertTo-Json -Compress
"#
    );
    serde_json::from_str(&run_ps(&body)?)
        .map_err(|e| runtime_error(format!("Invalid desktop bridge output: {e}")))
}

pub fn displays(_args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        let out = run_ps(
            r#"
$all=@([System.Windows.Forms.Screen]::AllScreens)
$items=@()
for($i=0;$i -lt $all.Count;$i++){ $s=$all[$i]; $items += [pscustomobject]@{ id=$i; name=$s.DeviceName; x=$s.Bounds.X; y=$s.Bounds.Y; width=$s.Bounds.Width; height=$s.Bounds.Height; primary=$s.Primary } }
[pscustomobject]@{ coordinate_space='display_local_physical_pixels'; displays=$items; returned_count=$items.Count } | ConvertTo-Json -Depth 4 -Compress
"#,
        )?;
        let value: Value = serde_json::from_str(&out)
            .map_err(|e| runtime_error(format!("Invalid desktop bridge output: {e}")))?;
        return Ok(tool_ok(value));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

pub fn screenshot(args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        let display = display_json(args.get("display_id").and_then(Value::as_u64))?;
        let quality = args.get("quality").and_then(Value::as_u64).unwrap_or(80);
        if !(1..=100).contains(&quality) {
            return Err(WorkspaceError::invalid_argument(
                "quality must be between 1 and 100",
            ));
        }
        let (image_width, image_height) = screenshot_dimensions(&display, args)?;
        let body = format!(
            r#"
$x={x};$y={y};$w={w};$h={h};$iw={image_width};$ih={image_height};$quality={quality}
$bmp=New-Object System.Drawing.Bitmap($w,$h,[System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
$g=[System.Drawing.Graphics]::FromImage($bmp)
$out=$bmp;$draw=$null;$ms=$null
try {{
  $g.CopyFromScreen($x,$y,0,0,(New-Object System.Drawing.Size($w,$h)),[System.Drawing.CopyPixelOperation]::SourceCopy)
  if($iw -ne $w -or $ih -ne $h) {{
    $out=New-Object System.Drawing.Bitmap($iw,$ih,[System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $draw=[System.Drawing.Graphics]::FromImage($out)
    $draw.InterpolationMode=[System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $draw.DrawImage($bmp,0,0,$iw,$ih)
  }}
  $ms=New-Object System.IO.MemoryStream
  $codec=[System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object MimeType -eq 'image/jpeg'
  $ep=New-Object System.Drawing.Imaging.EncoderParameters(1)
  $ep.Param[0]=New-Object System.Drawing.Imaging.EncoderParameter([System.Drawing.Imaging.Encoder]::Quality,[long]$quality)
  $out.Save($ms,$codec,$ep)
  [Convert]::ToBase64String($ms.ToArray())
}} finally {{
  if($draw){{$draw.Dispose()}}
  if($out -ne $bmp){{$out.Dispose()}}
  $g.Dispose();$bmp.Dispose()
  if($ms){{$ms.Dispose()}}
}}
"#,
            x = display["x"],
            y = display["y"],
            w = display["width"],
            h = display["height"],
        );
        let encoded = run_ps(&body)?;
        if encoded.is_empty() {
            return Err(runtime_error(
                "Desktop screenshot bridge returned an empty JPEG",
            ));
        }
        let bytes = base64_decoded_len(&encoded)?;
        let reference = json!({
            "id": display["id"], "name": display["name"],
            "x": display["x"], "y": display["y"],
            "width": display["width"], "height": display["height"],
            "image_width": image_width, "image_height": image_height
        });
        return Ok(tool_ok(json!({
            "display_id": display["id"], "display_name": display["name"],
            "x": display["x"], "y": display["y"], "width": display["width"], "height": display["height"], "primary": display["primary"],
            "image_width": image_width, "image_height": image_height,
            "coordinate_reference": reference,
            "coordinate_space": "display_local_physical_pixels", "image_coordinate_space": "image_pixels",
            "resized": image_width != display["width"].as_i64().unwrap_or(0) ||
                image_height != display["height"].as_i64().unwrap_or(0),
            "quality": quality,
            "mime_type": "image/jpeg", "bytes": bytes, "base64": encoded,
            "data_url": format!("data:image/jpeg;base64,{encoded}")
        })));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

fn screenshot_dimensions(display: &Value, args: &Value) -> Result<(i64, i64), WorkspaceError> {
    let width = display["width"].as_i64().unwrap_or(0);
    let height = display["height"].as_i64().unwrap_or(0);
    if width < 1 || height < 1 {
        return Err(WorkspaceError::invalid_argument(
            "invalid physical display dimensions",
        ));
    }
    let limit = |field: &str| -> Result<Option<i64>, WorkspaceError> {
        let Some(value) = args.get(field) else {
            return Ok(None);
        };
        let value = value.as_i64().ok_or_else(|| {
            WorkspaceError::invalid_argument(format!("{field} must be between 1 and 16384"))
        })?;
        if !(1..=16384).contains(&value) {
            return Err(WorkspaceError::invalid_argument(format!(
                "{field} must be between 1 and 16384"
            )));
        }
        Ok(Some(value))
    };
    let max_width = limit("max_width")?;
    let max_height = limit("max_height")?;
    let scale = 1.0_f64
        .min(max_width.map_or(1.0, |v| v as f64 / width as f64))
        .min(max_height.map_or(1.0, |v| v as f64 / height as f64));
    Ok((
        (width as f64 * scale).round().max(1.0) as i64,
        (height as f64 * scale).round().max(1.0) as i64,
    ))
}

fn required_i32(args: &Value, name: &str) -> Result<i32, WorkspaceError> {
    let value = args
        .get(name)
        .and_then(Value::as_i64)
        .ok_or_else(|| WorkspaceError::invalid_argument(format!("{name} is required")))?;
    i32::try_from(value)
        .map_err(|_| WorkspaceError::invalid_argument(format!("{name} is outside supported range")))
}

fn map_display_point(display: &Value, x: i32, y: i32) -> Result<(i32, i32), WorkspaceError> {
    map_display_point_with_reference(display, x, y, None)
}

fn map_display_point_with_reference(
    display: &Value,
    mut x: i32,
    mut y: i32,
    reference: Option<&Value>,
) -> Result<(i32, i32), WorkspaceError> {
    let w = display["width"].as_i64().unwrap_or(0) as i32;
    let h = display["height"].as_i64().unwrap_or(0) as i32;
    if let Some(reference) = reference {
        let required = |field: &str| -> Result<i64, WorkspaceError> {
            reference.get(field).and_then(Value::as_i64).ok_or_else(|| {
                WorkspaceError::invalid_argument(
                    "coordinate_reference must be copied from desktop_screenshot",
                )
            })
        };
        let iw = required("image_width")?;
        let ih = required("image_height")?;
        let id = required("id")?;
        let rx = required("x")?;
        let ry = required("y")?;
        let rw = required("width")?;
        let rh = required("height")?;
        let name = reference
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WorkspaceError::invalid_argument(
                    "coordinate_reference must be copied from desktop_screenshot",
                )
            })?;
        if iw < 1 || ih < 1 || id < 0 || rw < 1 || rh < 1 {
            return Err(WorkspaceError::invalid_argument(
                "coordinate_reference must be copied from desktop_screenshot",
            ));
        }
        if display["id"].as_i64() != Some(id)
            || display["name"].as_str() != Some(name)
            || display["x"].as_i64() != Some(rx)
            || display["y"].as_i64() != Some(ry)
            || display["width"].as_i64() != Some(rw)
            || display["height"].as_i64() != Some(rh)
        {
            return Err(WorkspaceError::invalid_argument(
                "Display changed since screenshot; take a new screenshot before using image coordinates"
            ));
        }
        if x < 0 || y < 0 || x as i64 >= iw || y as i64 >= ih {
            return Err(WorkspaceError::invalid_argument(format!(
                "Point ({x}, {y}) is outside screenshot bounds {iw}x{ih}"
            )));
        }
        x = if iw == 1 {
            w / 2
        } else {
            ((x as f64 * (w - 1) as f64) / (iw - 1) as f64).round() as i32
        };
        y = if ih == 1 {
            h / 2
        } else {
            ((y as f64 * (h - 1) as f64) / (ih - 1) as f64).round() as i32
        };
    }
    if x < 0 || y < 0 || x >= w || y >= h {
        return Err(WorkspaceError::invalid_argument(format!(
            "Point ({x}, {y}) is outside display bounds {w}x{h}"
        )));
    }
    let gx = display["x"].as_i64().unwrap_or(0) as i32 + x;
    let gy = display["y"].as_i64().unwrap_or(0) as i32 + y;
    Ok((gx, gy))
}

fn mouse_button_flags(button: &str) -> Result<(u32, u32), WorkspaceError> {
    match button {
        "left" => Ok((2, 4)),
        "right" => Ok((8, 16)),
        "middle" => Ok((32, 64)),
        _ => Err(WorkspaceError::invalid_argument("unsupported mouse button")),
    }
}

fn click_input_script(gx: i32, gy: i32, down: u32, up: u32, clicks: u64) -> String {
    format!("[void][ComputerUseNative]::SetCursorPos({gx},{gy}); 1..{clicks} | ForEach-Object {{ [ComputerUseNative]::mouse_event({down},0,0,0,[UIntPtr]::Zero); [ComputerUseNative]::mouse_event({up},0,0,0,[UIntPtr]::Zero) }}")
}

fn drag_input_script(
    from_gx: i32,
    from_gy: i32,
    to_gx: i32,
    to_gy: i32,
    down: u32,
    up: u32,
    duration_ms: u64,
    steps: u64,
) -> String {
    format!("[ComputerUseNative]::Drag({from_gx},{from_gy},{to_gx},{to_gy},{down},{up},{duration_ms},{steps})")
}

fn scroll_input_script(prefix: &str, dx: i32, dy: i32) -> String {
    format!("{prefix} if({dy} -ne 0){{[ComputerUseNative]::Wheel(2048,{dy})}}; if({dx} -ne 0){{[ComputerUseNative]::Wheel(4096,{dx})}}")
}

fn type_payload(text: &str) -> (String, usize) {
    (
        STANDARD.encode(text.as_bytes()),
        text.encode_utf16().count(),
    )
}

fn type_input_script(encoded: &str) -> String {
    format!("$t=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded}')); [ComputerUseNative]::TypeUnicode($t)")
}

fn base64_decoded_len(encoded: &str) -> Result<usize, WorkspaceError> {
    if encoded.len() % 4 != 0 {
        return Err(runtime_error(
            "Desktop bridge returned invalid base64 length",
        ));
    }
    let padding = encoded
        .as_bytes()
        .iter()
        .rev()
        .take_while(|&&byte| byte == b'=')
        .count();
    if padding > 2 {
        return Err(runtime_error(
            "Desktop bridge returned invalid base64 padding",
        ));
    }
    encoded
        .len()
        .checked_div(4)
        .and_then(|groups| groups.checked_mul(3))
        .and_then(|bytes| bytes.checked_sub(padding))
        .ok_or_else(|| runtime_error("Desktop bridge returned invalid base64 payload"))
}

#[cfg(target_os = "windows")]
fn global_point(args: &Value) -> Result<(Value, i32, i32), WorkspaceError> {
    let display = display_json(args.get("display_id").and_then(Value::as_u64))?;
    let x = required_i32(args, "x")?;
    let y = required_i32(args, "y")?;
    let (gx, gy) =
        map_display_point_with_reference(&display, x, y, args.get("coordinate_reference"))?;
    Ok((display, gx, gy))
}

pub fn click(args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        let (display, gx, gy) = global_point(args)?;
        let button = args.get("button").and_then(Value::as_str).unwrap_or("left");
        let clicks = args.get("clicks").and_then(Value::as_u64).unwrap_or(1);
        if !(1..=3).contains(&clicks) {
            return Err(WorkspaceError::invalid_argument(
                "clicks must be between 1 and 3",
            ));
        }
        let (down, up) = mouse_button_flags(button)?;
        run_ps(&click_input_script(gx, gy, down, up, clicks))?;
        let mut result = json!({
            "display_id":display["id"],
            "x": gx - display["x"].as_i64().unwrap_or(0) as i32,
            "y": gy - display["y"].as_i64().unwrap_or(0) as i32,
            "global_x":gx,"global_y":gy,"button":button,"clicks":clicks
        });
        if args.get("coordinate_reference").is_some() {
            result["input_x"] = json!(required_i32(args, "x")?);
            result["input_y"] = json!(required_i32(args, "y")?);
            result["coordinate_space"] = json!("image_pixels");
        }
        return Ok(tool_ok(result));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

pub fn drag(args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        let (display, from_gx, from_gy) = global_point(args)?;
        let to_display_id = args
            .get("to_display_id")
            .and_then(Value::as_u64)
            .or_else(|| display["id"].as_u64());
        let to_display = display_json(to_display_id)?;
        let to_x = required_i32(args, "to_x")?;
        let to_y = required_i32(args, "to_y")?;
        let to_reference = args.get("to_coordinate_reference").or_else(|| {
            if to_display["id"] == display["id"] {
                args.get("coordinate_reference")
            } else {
                None
            }
        });
        if args.get("coordinate_reference").is_some()
            && to_display["id"] != display["id"]
            && to_reference.is_none()
        {
            return Err(WorkspaceError::invalid_argument(
                "to_coordinate_reference is required when dragging image coordinates across displays"
            ));
        }
        let (to_gx, to_gy) =
            map_display_point_with_reference(&to_display, to_x, to_y, to_reference)?;
        let button = args.get("button").and_then(Value::as_str).unwrap_or("left");
        let duration_ms = args
            .get("duration_ms")
            .and_then(Value::as_u64)
            .unwrap_or(300);
        let steps = args.get("steps").and_then(Value::as_u64).unwrap_or(12);
        if duration_ms > 5_000 {
            return Err(WorkspaceError::invalid_argument(
                "duration_ms must be between 0 and 5000",
            ));
        }
        if !(1..=120).contains(&steps) {
            return Err(WorkspaceError::invalid_argument(
                "steps must be between 1 and 120",
            ));
        }
        let (down, up) = mouse_button_flags(button)?;
        run_ps(&drag_input_script(
            from_gx,
            from_gy,
            to_gx,
            to_gy,
            down,
            up,
            duration_ms,
            steps,
        ))?;
        let mut result = json!({
            "display_id": display["id"],
            "x": from_gx - display["x"].as_i64().unwrap_or(0) as i32,
            "y": from_gy - display["y"].as_i64().unwrap_or(0) as i32,
            "global_x": from_gx,
            "global_y": from_gy,
            "to_display_id": to_display["id"],
            "to_x": to_gx - to_display["x"].as_i64().unwrap_or(0) as i32,
            "to_y": to_gy - to_display["y"].as_i64().unwrap_or(0) as i32,
            "to_global_x": to_gx,
            "to_global_y": to_gy,
            "button": button,
            "duration_ms": duration_ms,
            "steps": steps
        });
        if args.get("coordinate_reference").is_some() {
            result["input_x"] = json!(required_i32(args, "x")?);
            result["input_y"] = json!(required_i32(args, "y")?);
        }
        if to_reference.is_some() {
            result["input_to_x"] = json!(to_x);
            result["input_to_y"] = json!(to_y);
        }
        return Ok(tool_ok(result));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

pub fn scroll(args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        if args.get("coordinate_reference").is_some()
            && (args.get("x").is_none() || args.get("y").is_none())
        {
            return Err(WorkspaceError::invalid_argument(
                "x and y are required with coordinate_reference",
            ));
        }
        let dy = required_i32(args, "delta_y")?;
        let dx = args.get("delta_x").and_then(Value::as_i64).unwrap_or(0) as i32;
        let mut prefix = String::new();
        if args.get("x").is_some() || args.get("y").is_some() {
            let (_, gx, gy) = global_point(args)?;
            prefix = format!("[void][ComputerUseNative]::SetCursorPos({gx},{gy});");
        }
        run_ps(&scroll_input_script(&prefix, dx, dy))?;
        return Ok(tool_ok(json!({"delta_x":dx,"delta_y":dy})));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

pub fn type_text(args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| WorkspaceError::invalid_argument("text is required"))?;
        let (b64, utf16_units) = type_payload(text);
        run_ps(&type_input_script(&b64))?;
        return Ok(tool_ok(json!({"typed_utf16_units":utf16_units})));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

pub fn key(args: &Value) -> Result<Value, WorkspaceError> {
    #[cfg(target_os = "windows")]
    {
        let values = args
            .get("keys")
            .and_then(Value::as_array)
            .ok_or_else(|| WorkspaceError::invalid_argument("keys is required"))?;
        if values.is_empty() || values.len() > 8 {
            return Err(WorkspaceError::invalid_argument(
                "keys must contain 1 to 8 items",
            ));
        }
        let mut names = Vec::new();
        let mut codes = Vec::new();
        for value in values {
            let name = value
                .as_str()
                .ok_or_else(|| WorkspaceError::invalid_argument("keys must contain strings"))?;
            names.push(name.to_string());
            codes.push(key_code(name).ok_or_else(|| {
                WorkspaceError::invalid_argument(format!("unsupported key: {name}"))
            })?);
        }
        let csv = codes
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        run_ps(&format!("[ComputerUseNative]::Hotkey([uint16[]]@({csv}))"))?;
        return Ok(tool_ok(json!({"keys":names})));
    }
    #[allow(unreachable_code)]
    Err(unsupported())
}

fn key_code(name: &str) -> Option<u16> {
    let upper = name.trim().to_ascii_uppercase();
    if upper.len() == 1 {
        let b = upper.as_bytes()[0];
        if b.is_ascii_alphanumeric() {
            return Some(b as u16);
        }
    }
    if let Some(number) = upper
        .strip_prefix('F')
        .and_then(|value| value.parse::<u16>().ok())
    {
        if (1..=24).contains(&number) {
            return Some(0x6F + number);
        }
    }
    match upper.as_str() {
        "CTRL" | "CONTROL" => Some(0x11),
        "ALT" => Some(0x12),
        "SHIFT" => Some(0x10),
        "WIN" | "META" | "SUPER" => Some(0x5B),
        "ENTER" | "RETURN" => Some(0x0D),
        "TAB" => Some(0x09),
        "ESC" | "ESCAPE" => Some(0x1B),
        "BACKSPACE" => Some(0x08),
        "DELETE" | "DEL" => Some(0x2E),
        "LEFT" => Some(0x25),
        "UP" => Some(0x26),
        "RIGHT" => Some(0x27),
        "DOWN" => Some(0x28),
        "HOME" => Some(0x24),
        "END" => Some(0x23),
        "PAGEUP" | "PGUP" => Some(0x21),
        "PAGEDOWN" | "PGDN" => Some(0x22),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use serde_json::json;

    use super::{
        base64_decoded_len, click_input_script, drag_input_script, key_code, map_display_point,
        map_display_point_with_reference, mouse_button_flags, screenshot_dimensions,
        scroll_input_script, type_input_script, type_payload,
    };

    #[cfg(target_os = "windows")]
    #[test]
    fn desktop_bridge_runs_without_a_visible_console() {
        let visibility = super::run_ps(
            r#"Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class DesktopConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr handle); }'
$handle = [DesktopConsoleProbe]::GetConsoleWindow()
if ($handle -eq [IntPtr]::Zero -or -not [DesktopConsoleProbe]::IsWindowVisible($handle)) { 'hidden' } else { 'visible' }"#,
        )
        .expect("PowerShell desktop bridge");
        assert_eq!(
            visibility, "hidden",
            "desktop bridge exposed a console window"
        );
    }

    #[test]
    fn common_key_codes_are_stable() {
        assert_eq!(key_code("ctrl"), Some(0x11));
        assert_eq!(key_code("A"), Some(0x41));
        assert_eq!(key_code("pageDown"), Some(0x22));
        assert_eq!(key_code("F1"), Some(0x70));
        assert_eq!(key_code("f12"), Some(0x7B));
        assert_eq!(key_code("F24"), Some(0x87));
        assert_eq!(key_code("F25"), None);
        assert_eq!(key_code("wat"), None);
    }

    #[test]
    fn desktop_point_mapping_preserves_display_local_physical_pixels() {
        let left = json!({"x": -2560, "y": 180, "width": 2560, "height": 1440});
        assert_eq!(
            map_display_point(&left, 125, 75).expect("valid point"),
            (-2435, 255)
        );

        let upper_right = json!({"x": 1920, "y": -1200, "width": 1600, "height": 1200});
        assert_eq!(
            map_display_point(&upper_right, 10, 20).expect("valid point"),
            (1930, -1180)
        );
    }

    #[test]
    fn desktop_point_mapping_rejects_out_of_bounds_coordinates() {
        let display = json!({"x": 0, "y": 0, "width": 1920, "height": 1080});
        for (x, y) in [(-1, 0), (0, -1), (1920, 0), (0, 1080)] {
            assert!(
                map_display_point(&display, x, y).is_err(),
                "expected ({x}, {y}) to be rejected"
            );
        }
    }

    #[test]
    fn downscaled_screenshot_coordinates_map_to_screen_edges_and_reject_stale_displays() {
        let display = json!({
            "id": 1, "name": "DISPLAY2", "x": -2560, "y": 180,
            "width": 2560, "height": 1440
        });
        assert_eq!(
            screenshot_dimensions(&display, &json!({})).unwrap(),
            (2560, 1440)
        );
        assert_eq!(
            screenshot_dimensions(&display, &json!({"max_width":1280})).unwrap(),
            (1280, 720)
        );
        assert_eq!(
            screenshot_dimensions(&display, &json!({"max_width":1280, "max_height":360})).unwrap(),
            (640, 360)
        );
        for bad in [
            json!({"max_width":0}),
            json!({"max_width":-1}),
            json!({"max_height":16385}),
        ] {
            assert!(screenshot_dimensions(&display, &bad).is_err());
        }
        let reference = json!({
            "id": 1, "name": "DISPLAY2", "x": -2560, "y": 180,
            "width": 2560, "height": 1440, "image_width": 1280, "image_height": 720
        });
        assert_eq!(
            map_display_point_with_reference(&display, 500, 300, Some(&reference)).unwrap(),
            (-1560, 780)
        );
        assert_eq!(
            map_display_point_with_reference(&display, 1279, 719, Some(&reference)).unwrap(),
            (-1, 1619)
        );
        assert!(map_display_point_with_reference(&display, 1280, 0, Some(&reference)).is_err());
        let changed = json!({
            "id": 1, "name": "DISPLAY2", "x": -2560, "y": 180,
            "width": 1920, "height": 1080
        });
        assert!(
            map_display_point_with_reference(&changed, 500, 300, Some(&reference))
                .unwrap_err()
                .message()
                .contains("Display changed since screenshot")
        );
        let moved = json!({
            "id": 1, "name": "DISPLAY2", "x": 0, "y": 180,
            "width": 2560, "height": 1440
        });
        assert!(map_display_point_with_reference(&moved, 500, 300, Some(&reference)).is_err());
        assert!(map_display_point_with_reference(
            &display,
            500,
            300,
            Some(&json!({"image_width":0}))
        )
        .is_err());
    }

    #[test]
    fn click_input_mapping_is_stable_without_calling_user32() {
        assert_eq!(mouse_button_flags("left").expect("left"), (2, 4));
        assert_eq!(mouse_button_flags("right").expect("right"), (8, 16));
        assert_eq!(mouse_button_flags("middle").expect("middle"), (32, 64));
        assert!(mouse_button_flags("side").is_err());

        let script = click_input_script(-2435, 255, 8, 16, 2);
        assert!(script.contains("SetCursorPos(-2435,255)"));
        assert!(script.contains("1..2"));
        assert!(script.contains("mouse_event(8,0,0,0"));
        assert!(script.contains("mouse_event(16,0,0,0"));
    }

    #[test]
    fn drag_input_mapping_is_stable_without_calling_user32() {
        assert_eq!(
            drag_input_script(-2435, 255, -1760, 680, 2, 4, 450, 18),
            "[ComputerUseNative]::Drag(-2435,255,-1760,680,2,4,450,18)"
        );
    }

    #[test]
    fn scroll_input_preserves_signed_wheel_deltas() {
        let script = scroll_input_script(
            "[void][ComputerUseNative]::SetCursorPos(-2435,255);",
            -240,
            -1200,
        );
        assert!(script.contains("SetCursorPos(-2435,255)"));
        assert!(script.contains("Wheel(2048,-1200)"));
        assert!(script.contains("Wheel(4096,-240)"));
        assert!(!script.contains("[uint32]"));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn screenshot_bridge_returns_real_jpeg_when_opted_in() {
        if std::env::var("CTMCP_DESKTOP_SCREENSHOT_SMOKE").as_deref() != Ok("1") {
            return;
        }
        let capture = super::screenshot(&json!({"max_width": 640, "quality": 65}))
            .expect("Windows screenshot must produce a non-empty JPEG");
        assert_eq!(capture["ok"], true);
        assert!(capture["bytes"].as_u64().unwrap_or(0) > 100);
        assert!(capture["base64"].as_str().unwrap_or("").starts_with("/9j/"));
    }

    #[test]
    fn base64_byte_count_handles_padding_exactly() {
        assert_eq!(base64_decoded_len("").expect("empty payload"), 0);
        assert_eq!(base64_decoded_len("Zg==").expect("one byte"), 1);
        assert_eq!(base64_decoded_len("Zm8=").expect("two bytes"), 2);
        assert_eq!(base64_decoded_len("Zm9v").expect("three bytes"), 3);
        assert_eq!(base64_decoded_len("AAECAwQ=").expect("five bytes"), 5);
        assert!(base64_decoded_len("abc").is_err());
        assert!(base64_decoded_len("====").is_err());
    }

    #[test]
    fn type_payload_round_trips_unicode_without_embedding_raw_text() {
        let text = "A中😀\nquote:'\" & | < >";
        let (encoded, units) = type_payload(text);
        assert_eq!(
            STANDARD.decode(&encoded).expect("valid base64"),
            text.as_bytes()
        );
        assert_eq!(units, text.encode_utf16().count());
        assert_eq!(type_payload("😀").1, 2);

        let script = type_input_script(&encoded);
        assert!(script.contains("FromBase64String"));
        assert!(script.contains(&encoded));
        assert!(!script.contains(text));
        assert!(script.contains("TypeUnicode($t)"));
    }
}
