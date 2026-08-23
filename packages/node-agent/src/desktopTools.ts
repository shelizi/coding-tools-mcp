import { spawnSync } from 'node:child_process';
import type { JsonObject } from './types.js';

type DesktopDisplay = {
  id: number;
  name?: string;
  x: number;
  y: number;
  width: number;
  height: number;
  primary?: boolean;
};

const PREAMBLE = String.raw`
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
  [DllImport("user32.dll",SetLastError=true)] public static extern bool SetCursorPos(int x, int y);
  [DllImport("user32.dll")] public static extern void mouse_event(uint flags,uint dx,uint dy,uint data,UIntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct INPUT { public uint type; public InputUnion U; }
  [StructLayout(LayoutKind.Sequential)] public struct MOUSEINPUT { public int dx; public int dy; public uint mouseData; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct KEYBDINPUT { public ushort wVk; public ushort wScan; public uint dwFlags; public uint time; public UIntPtr dwExtraInfo; }
  [StructLayout(LayoutKind.Sequential)] public struct HARDWAREINPUT { public uint uMsg; public ushort wParamL; public ushort wParamH; }
  [StructLayout(LayoutKind.Explicit)] public struct InputUnion { [FieldOffset(0)] public MOUSEINPUT mi; [FieldOffset(0)] public KEYBDINPUT ki; [FieldOffset(0)] public HARDWAREINPUT hi; }
  [DllImport("user32.dll",SetLastError=true)] public static extern uint SendInput(uint count,INPUT[] inputs,int size);
  static int InputSize() { int size=Marshal.SizeOf(typeof(INPUT)); int expected=IntPtr.Size==8?40:28; if(size!=expected) throw new InvalidOperationException("Unexpected INPUT size: "+size+" (expected "+expected+")"); return size; }
  static void Send(INPUT[] inputs) { uint sent=SendInput((uint)inputs.Length,inputs,InputSize()); if(sent!=inputs.Length) throw new InvalidOperationException("SendInput failed with Win32 error "+Marshal.GetLastWin32Error()); }
  static void Move(int x,int y) { if(!SetCursorPos(x,y)) throw new InvalidOperationException("SetCursorPos failed with Win32 error "+Marshal.GetLastWin32Error()); }
  public static void TypeUnicode(string text) { foreach(char ch in text) { var d=new INPUT { type=1,U=new InputUnion { ki=new KEYBDINPUT { wScan=ch,dwFlags=4 } } }; var u=new INPUT { type=1,U=new InputUnion { ki=new KEYBDINPUT { wScan=ch,dwFlags=6 } } }; Send(new INPUT[]{d,u}); } }
  public static void Hotkey(ushort[] keys) { var a=new INPUT[keys.Length*2]; int i=0; foreach(ushort k in keys) a[i++]=new INPUT { type=1,U=new InputUnion { ki=new KEYBDINPUT { wVk=k } } }; for(int n=keys.Length-1;n>=0;n--) a[i++]=new INPUT { type=1,U=new InputUnion { ki=new KEYBDINPUT { wVk=keys[n],dwFlags=2 } } }; Send(a); }
  public static void Wheel(uint flag,int delta) { mouse_event(flag,0,0,unchecked((uint)delta),UIntPtr.Zero); }
  public static void Drag(int fromX,int fromY,int toX,int toY,uint down,uint up,int durationMs,int steps) { Move(fromX,fromY); mouse_event(down,0,0,0,UIntPtr.Zero); try { int delay=steps>0?durationMs/steps:0; for(int i=1;i<=steps;i++) { double t=i/(double)steps; int x=fromX+(int)Math.Round((toX-fromX)*t); int y=fromY+(int)Math.Round((toY-fromY)*t); Move(x,y); if(delay>0) Thread.Sleep(delay); } } finally { mouse_event(up,0,0,0,UIntPtr.Zero); } }
}
'@
}
[void][ComputerUseNative]::SetThreadDpiAwarenessContext([IntPtr](-4))
`;

function ok(extra: JsonObject): JsonObject { return { ok: true, status: 'ok', ...extra }; }
function fail(message: string): never { throw new Error(message); }
function numberArg(args: JsonObject, name: string, required = true): number | undefined {
  const value = args[name];
  if (value === undefined && !required) return undefined;
  if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isInteger(value)) fail(`${name} must be an integer`);
  return value as number;
}
export function mapDisplayPoint(d: DesktopDisplay, x: number, y: number): { x:number,y:number,gx:number,gy:number } {
  if (!Number.isInteger(x) || !Number.isInteger(y)) fail('desktop coordinates must be integers');
  if (x<0||y<0||x>=d.width||y>=d.height) fail(`Point (${x}, ${y}) is outside display bounds ${d.width}x${d.height}`);
  return {x,y,gx:d.x+x,gy:d.y+y};
}

export function desktopTypePayload(text: string): { base64:string,typedUtf16Units:number } {
  return { base64:Buffer.from(text,'utf8').toString('base64'), typedUtf16Units:text.length };
}
export function desktopScrollScript(prefix: string, dx: number, dy: number): string {
  return `${prefix}if(${dy} -ne 0){[ComputerUseNative]::Wheel(2048,${dy})};if(${dx} -ne 0){[ComputerUseNative]::Wheel(4096,${dx})}`;
}
function runPs(body: string): string {
  if (process.platform !== 'win32') fail('Desktop computer-use tools are currently supported on Windows only.');
  const result = spawnSync('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-Command','-'],{input:`${PREAMBLE}\n${body}`,encoding:'utf8',maxBuffer:32*1024*1024,windowsHide:true});
  if (result.status !== 0) fail(`Desktop bridge failed: ${(result.stderr || '').trim()}`);
  return (result.stdout || '').trim();
}
function display(displayId?: number): any {
  const id = displayId === undefined ? '$null' : String(displayId);
  return JSON.parse(runPs(`$all=@([System.Windows.Forms.Screen]::AllScreens);$items=@();for($i=0;$i -lt $all.Count;$i++){$s=$all[$i];$items += [pscustomobject]@{id=$i;name=$s.DeviceName;x=$s.Bounds.X;y=$s.Bounds.Y;width=$s.Bounds.Width;height=$s.Bounds.Height;primary=$s.Primary}};$id=${id};if($null -eq $id){$selected=$items|Where-Object primary|Select-Object -First 1;if($null -eq $selected){$selected=$items[0]}}else{$selected=$items|Where-Object id -eq $id|Select-Object -First 1};if($null -eq $selected){throw 'Unknown display_id'};$selected|ConvertTo-Json -Compress`));
}
function point(args: JsonObject): { d:any,x:number,y:number,gx:number,gy:number } {
  const d=display(numberArg(args,'display_id',false)); const x=numberArg(args,'x')!; const y=numberArg(args,'y')!;
  return {d,...mapDisplayPoint(d,x,y)};
}
function mouseButtonFlags(button: string): [number,number] {
  const flags:Record<string,[number,number]>={left:[2,4],right:[8,16],middle:[32,64]}; const pair=flags[button]; if(!pair) fail('unsupported mouse button'); return pair;
}

export function desktopDisplays(): JsonObject {
  const parsed=JSON.parse(runPs(`$all=@([System.Windows.Forms.Screen]::AllScreens);$items=@();for($i=0;$i -lt $all.Count;$i++){$s=$all[$i];$items += [pscustomobject]@{id=$i;name=$s.DeviceName;x=$s.Bounds.X;y=$s.Bounds.Y;width=$s.Bounds.Width;height=$s.Bounds.Height;primary=$s.Primary}};[pscustomobject]@{coordinate_space='display_local_physical_pixels';displays=$items;returned_count=$items.Count}|ConvertTo-Json -Depth 4 -Compress`));
  return ok(parsed);
}
export function desktopScreenshot(args: JsonObject): JsonObject {
  const d=display(numberArg(args,'display_id',false)); const quality=numberArg(args,'quality',false) ?? 80; if(quality<1||quality>100) fail('quality must be between 1 and 100');
  const base64=runPs(`$x=${d.x};$y=${d.y};$w=${d.width};$h=${d.height};$q=${quality};$bmp=New-Object System.Drawing.Bitmap($w,$h,[System.Drawing.Imaging.PixelFormat]::Format24bppRgb);$g=[System.Drawing.Graphics]::FromImage($bmp);try{$g.CopyFromScreen($x,$y,0,0,(New-Object System.Drawing.Size($w,$h)),[System.Drawing.CopyPixelOperation]::SourceCopy);$ms=New-Object System.IO.MemoryStream;$codec=[System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders()|Where-Object MimeType -eq 'image/jpeg';$ep=New-Object System.Drawing.Imaging.EncoderParameters(1);$ep.Param[0]=New-Object System.Drawing.Imaging.EncoderParameter([System.Drawing.Imaging.Encoder]::Quality,[long]$q);$bmp.Save($ms,$codec,$ep);[Convert]::ToBase64String($ms.ToArray())}finally{$g.Dispose();$bmp.Dispose();if($ms){$ms.Dispose()}}`);
  return ok({display_id:d.id,display_name:d.name,x:d.x,y:d.y,width:d.width,height:d.height,primary:d.primary,coordinate_space:'display_local_physical_pixels',resized:false,quality,mime_type:'image/jpeg',bytes:Buffer.byteLength(base64,'base64'),base64,data_url:`data:image/jpeg;base64,${base64}`});
}
export function desktopClick(args: JsonObject): JsonObject {
  const p=point(args); const button=typeof args.button==='string'?args.button:'left'; const clicks=numberArg(args,'clicks',false) ?? 1; if(clicks<1||clicks>3) fail('clicks must be between 1 and 3'); const pair=mouseButtonFlags(button);
  runPs(`[void][ComputerUseNative]::SetCursorPos(${p.gx},${p.gy});1..${clicks}|ForEach-Object{[ComputerUseNative]::mouse_event(${pair[0]},0,0,0,[UIntPtr]::Zero);[ComputerUseNative]::mouse_event(${pair[1]},0,0,0,[UIntPtr]::Zero)}`); return ok({display_id:p.d.id,x:p.x,y:p.y,global_x:p.gx,global_y:p.gy,button,clicks});
}
export function desktopDrag(args: JsonObject): JsonObject {
  const start=point(args); const toDisplayId=numberArg(args,'to_display_id',false); const endDisplay=display(toDisplayId ?? start.d.id); const toX=numberArg(args,'to_x')!; const toY=numberArg(args,'to_y')!; const end=mapDisplayPoint(endDisplay,toX,toY);
  const button=typeof args.button==='string'?args.button:'left'; const [down,up]=mouseButtonFlags(button); const durationMs=numberArg(args,'duration_ms',false) ?? 300; const steps=numberArg(args,'steps',false) ?? 12;
  if(durationMs<0||durationMs>5000) fail('duration_ms must be between 0 and 5000'); if(steps<1||steps>120) fail('steps must be between 1 and 120');
  runPs(`[ComputerUseNative]::Drag(${start.gx},${start.gy},${end.gx},${end.gy},${down},${up},${durationMs},${steps})`);
  return ok({display_id:start.d.id,x:start.x,y:start.y,global_x:start.gx,global_y:start.gy,to_display_id:endDisplay.id,to_x:end.x,to_y:end.y,to_global_x:end.gx,to_global_y:end.gy,button,duration_ms:durationMs,steps});
}
export function desktopScroll(args: JsonObject): JsonObject {
  const dy=numberArg(args,'delta_y')!; const dx=numberArg(args,'delta_x',false) ?? 0; let prefix=''; if(args.x!==undefined||args.y!==undefined){const p=point(args);prefix=`[void][ComputerUseNative]::SetCursorPos(${p.gx},${p.gy});`;}
  runPs(desktopScrollScript(prefix,dx,dy)); return ok({delta_x:dx,delta_y:dy});
}
export function desktopType(args: JsonObject): JsonObject {
  if(typeof args.text!=='string') fail('text is required'); const payload=desktopTypePayload(args.text); runPs(`$t=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${payload.base64}'));[ComputerUseNative]::TypeUnicode($t)`); return ok({typed_utf16_units:payload.typedUtf16Units});
}
export function desktopKeyCode(name:string):number|undefined { const u=name.trim().toUpperCase(); if(/^[A-Z0-9]$/.test(u)) return u.charCodeAt(0); const functionKey=/^F([1-9]|1[0-9]|2[0-4])$/.exec(u); if(functionKey) return 0x6f+Number(functionKey[1]); const m:Record<string,number>={CTRL:0x11,CONTROL:0x11,ALT:0x12,SHIFT:0x10,WIN:0x5b,META:0x5b,SUPER:0x5b,ENTER:0x0d,RETURN:0x0d,TAB:0x09,ESC:0x1b,ESCAPE:0x1b,BACKSPACE:0x08,DELETE:0x2e,DEL:0x2e,LEFT:0x25,UP:0x26,RIGHT:0x27,DOWN:0x28,HOME:0x24,END:0x23,PAGEUP:0x21,PGUP:0x21,PAGEDOWN:0x22,PGDN:0x22}; return m[u]; }
export function desktopKey(args: JsonObject): JsonObject { if(!Array.isArray(args.keys)||args.keys.length<1||args.keys.length>8||args.keys.some(k=>typeof k!=='string')) fail('keys must contain 1 to 8 strings'); const keys=args.keys as string[]; const codes=keys.map(k=>desktopKeyCode(k) ?? fail(`unsupported key: ${k}`)); runPs(`[ComputerUseNative]::Hotkey([uint16[]]@(${codes.join(',')}))`); return ok({keys}); }
