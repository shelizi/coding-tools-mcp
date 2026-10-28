import { desktopKeyCode, display, fail, mapDisplayPoint, numberArg, runPs, desktopTypePayload, desktopScreenshot } from './desktopTools.js';
import type { JsonObject } from './types.js';

type Action = JsonObject;
type Screen = { id: number; name: string; x: number; y: number; width: number; height: number };
type Point = { gx: number; gy: number; displayId: number };
export type DesktopInputPlan = { script: string; actionCount: number; displayCount: number };

function integer(action: Action, name: string, min: number, max: number, fallback?: number): number {
  const value = numberArg(action, name, fallback === undefined);
  const result = value ?? fallback;
  if (result === undefined || result < min || result > max) fail(`${name} must be between ${min} and ${max}`);
  return result;
}

function keys(action: Action, field = 'keys'): number[] {
  const value = action[field];
  if (!Array.isArray(value) || value.length < 1 || value.length > 8 ||
      value.some(key => typeof key !== 'string')) fail(`${field} must contain 1 to 8 key names`);
  const result = value.map(name => desktopKeyCode(name) ?? fail(`unsupported key: ${name}`));
  if (new Set(result).size !== result.length) fail(`${field} cannot contain repeated keys`);
  return result;
}

function button(action: Action): [number, number] {
  const value = action.button ?? 'left';
  if (value === 'left') return [2, 4];
  if (value === 'right') return [8, 16];
  if (value === 'middle') return [32, 64];
  return fail('button must be left, right or middle');
}

/** Pure validation and scripting: rejects an entire invalid batch before any mouse or key event. */
export function planDesktopInput(
  args: JsonObject,
  getDisplay: (id?: number) => Screen = display
): DesktopInputPlan {
  const actions = args.actions;
  if (!Array.isArray(actions) || actions.length < 1 || actions.length > 64) {
    fail('actions must contain 1 to 64 operations');
  }
  const activeKeys = new Set<number>();
  const activeButtons = new Set<number>();
  const screens = new Map<string, Screen>();
  const instructions: string[] = [];
  let totalDuration = 0;

  const point = (action: Action, target = false, fallbackId?: number, fallbackReference?: unknown): Point => {
    const idKey = target ? 'to_display_id' : 'display_id';
    const id = action[idKey] === undefined ? fallbackId : integer(action, idKey, 0, 64);
    const lookupKey = id === undefined ? 'primary' : String(id);
    let screen = screens.get(lookupKey);
    if (!screen) {
      screen = getDisplay(id);
      screens.set(lookupKey, screen);
    }
    const x = integer(action, target ? 'to_x' : 'x', 0, 1000000);
    const y = integer(action, target ? 'to_y' : 'y', 0, 1000000);
    const reference = action[target ? 'to_coordinate_reference' : 'coordinate_reference'] ?? fallbackReference;
    const mapped = mapDisplayPoint(screen, x, y, reference as Parameters<typeof mapDisplayPoint>[3]);
    return { gx: mapped.gx, gy: mapped.gy, displayId: screen.id };
  };
  const move = (point: Point): string => `[ComputerUseNative]::Move(${point.gx},${point.gy})`;
  const downKey = (key: number): string =>
    `[ComputerUseNative]::KeyDown(${key});$heldKeys.Add(${key})`;
  const upKey = (key: number): string =>
    `[ComputerUseNative]::KeyUp(${key});[void]$heldKeys.Remove(${key})`;
  const downMouse = ([down, up]: [number,number]): string =>
    `[ComputerUseNative]::MouseDown(${down});$heldButtons.Add(${up})`;
  const upMouse = (up: number): string =>
    `[ComputerUseNative]::MouseUp(${up});[void]$heldButtons.Remove(${up})`;
  const scope = (action: Action, body: string[]): void => {
    const modifiers = action.modifiers === undefined ? [] : keys({ keys: action.modifiers });
    const introduced = modifiers.filter(key => !activeKeys.has(key));
    instructions.push(...introduced.map(downKey), ...body, ...introduced.reverse().map(upKey));
  };

  for (const [index, raw] of actions.entries()) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) fail(`actions[${index}] must be an object`);
    const action = raw as Action;
    if (typeof action.type !== 'string') fail(`actions[${index}].type is required`);
    switch (action.type) {
      case 'key_down': {
        const codes = keys(action);
        for (const code of codes) {
          if (activeKeys.has(code)) fail('key_down cannot hold a key twice');
          activeKeys.add(code);
          instructions.push(downKey(code));
        }
        break;
      }
      case 'key_up': {
        for (const code of keys(action).reverse()) {
          if (!activeKeys.delete(code)) fail('key_up without a matching key_down');
          instructions.push(upKey(code));
        }
        break;
      }
      case 'key': {
        const codes = keys(action);
        if (codes.some(code => activeKeys.has(code))) fail('key shortcut overlaps a held key; use key_down and key_up');
        instructions.push(`[ComputerUseNative]::Hotkey([uint16[]]@(${codes.join(',')}))`);
        break;
      }
      case 'type': {
        if (typeof action.text !== 'string' || action.text.length > 16384) fail('text must be a string up to 16384 characters');
        const payload = desktopTypePayload(action.text);
        instructions.push(`[ComputerUseNative]::TypeUnicode([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${payload.base64}')))`);
        break;
      }
      case 'move': {
        instructions.push(move(point(action)));
        break;
      }
      case 'mouse_down': {
        const pair = button(action);
        if (activeButtons.has(pair[1])) fail('mouse_down cannot hold a button twice');
        if (action.x !== undefined || action.y !== undefined) instructions.push(move(point(action)));
        activeButtons.add(pair[1]);
        instructions.push(downMouse(pair));
        break;
      }
      case 'mouse_up': {
        const pair = button(action);
        if (!activeButtons.delete(pair[1])) fail('mouse_up without matching mouse_down');
        if (action.x !== undefined || action.y !== undefined) instructions.push(move(point(action)));
        instructions.push(upMouse(pair[1]));
        break;
      }
      case 'click': {
        const pair = button(action);
        if (activeButtons.has(pair[1])) fail('click cannot reuse a held mouse button');
        const location = point(action);
        const count = integer(action, 'clicks', 1, 3, 1);
        const script = [move(location)];
        for (let n = 0; n < count; n++) script.push(downMouse(pair), upMouse(pair[1]));
        scope(action, script);
        break;
      }
      case 'drag': {
        const pair = button(action);
        if (activeButtons.has(pair[1])) fail('drag cannot reuse a held mouse button');
        const start = point(action);
        const locations: Point[] = [start];
        if (action.path !== undefined) {
          if (!Array.isArray(action.path) || action.path.length < 1 || action.path.length > 32 ||
              action.to_x !== undefined || action.to_y !== undefined) {
            fail('drag.path must contain 1 to 32 waypoints and cannot be combined with to_x/to_y');
          }
          for (const rawWaypoint of action.path) {
            if (!rawWaypoint || typeof rawWaypoint !== 'object' || Array.isArray(rawWaypoint)) fail('drag.path waypoint must be an object');
            const waypoint = rawWaypoint as Action;
            const reference = waypoint.coordinate_reference ?? (waypoint.display_id === undefined || waypoint.display_id === start.displayId ? action.coordinate_reference : undefined);
            if (action.coordinate_reference !== undefined &&
                waypoint.display_id !== undefined && waypoint.display_id !== start.displayId &&
                waypoint.coordinate_reference === undefined) fail('cross-display waypoints require coordinate_reference');
            locations.push(point(waypoint, false, start.displayId, reference));
          }
        } else {
          const endId = action.to_display_id === undefined ? start.displayId : integer(action, 'to_display_id', 0, 64);
          const fallbackReference = endId === start.displayId ? action.coordinate_reference : undefined;
          if (action.coordinate_reference !== undefined && endId !== start.displayId && action.to_coordinate_reference === undefined) {
            fail('cross-display drag requires to_coordinate_reference');
          }
          locations.push(point(action, true, start.displayId, fallbackReference));
        }
        const duration = integer(action, 'duration_ms', 0, 5000, 300);
        const steps = integer(action, 'steps', 1, 120, 12);
        totalDuration += duration;
        const coords = locations.flatMap(p => [p.gx, p.gy]).join(',');
        scope(action, [move(start), downMouse(pair),
          `[ComputerUseNative]::MovePath([int[]]@(${coords}),${duration},${steps})`,
          upMouse(pair[1])]);
        break;
      }
      case 'scroll': {
        const dx = integer(action, 'delta_x', -12000, 12000, 0);
        const dy = integer(action, 'delta_y', -12000, 12000, 0);
        if (!dx && !dy) fail('scroll requires nonzero delta_x or delta_y');
        const script: string[] = [];
        if (action.x !== undefined || action.y !== undefined) script.push(move(point(action)));
        if (dy) script.push(`[ComputerUseNative]::Wheel(2048,${dy})`);
        if (dx) script.push(`[ComputerUseNative]::Wheel(4096,${dx})`);
        scope(action, script);
        break;
      }
      case 'wait': {
        const duration = integer(action, 'duration_ms', 0, 2000);
        totalDuration += duration;
        instructions.push(`[System.Threading.Thread]::Sleep(${duration})`);
        break;
      }
      default:
        fail(`unsupported desktop action: ${action.type}`);
    }
    if (totalDuration > 20000) fail('desktop input duration exceeds 20000 ms');
  }

  // A cross-process named mutex prevents concurrent batches in the same
  // Windows logon session from interleaving modifier and pointer state.
  const checks: string[] = [];
  for (const screen of new Map([...screens.values()].map(s => [s.id, s])).values()) {
    const name = Buffer.from(screen.name, 'utf8').toString('base64');
    checks.push(`if($all.Count -le ${screen.id}){throw 'Display changed since snapshot'}`);
    checks.push(`$s=$all[${screen.id}];if($s.DeviceName -ne [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('${name}')) -or $s.Bounds.X -ne ${screen.x} -or $s.Bounds.Y -ne ${screen.y} -or $s.Bounds.Width -ne ${screen.width} -or $s.Bounds.Height -ne ${screen.height}){throw 'Display changed since snapshot'}`);
  }
  const script = [
    "$heldKeys=[System.Collections.Generic.List[int]]::new()",
    "$heldButtons=[System.Collections.Generic.List[int]]::new()",
    "$desktopMutex=[System.Threading.Mutex]::new($false,'Local\\CodingToolsMCPDesktopInput_v1')",
    "$locked=$false",
    "try {",
    "try{$locked=$desktopMutex.WaitOne(10000)}catch [System.Threading.AbandonedMutexException]{$locked=$true}",
    "if(-not $locked){throw 'Desktop input busy'}",
    "try {",
    ...(checks.length ? ["$all=@([System.Windows.Forms.Screen]::AllScreens)", ...checks] : []),
    ...instructions,
    "} finally {",
    "for($i=$heldButtons.Count-1;$i -ge 0;$i--){try{[ComputerUseNative]::MouseUp($heldButtons[$i])}catch{}}",
    "for($i=$heldKeys.Count-1;$i -ge 0;$i--){try{[ComputerUseNative]::KeyUp($heldKeys[$i])}catch{}}",
    "}",
    "} finally {if($locked){[void]$desktopMutex.ReleaseMutex()};$desktopMutex.Dispose()}"
  ].join('\n');
  return { script, actionCount: actions.length, displayCount: screens.size };
}

export function desktopInput(args: JsonObject): JsonObject {
  const plan = planDesktopInput(args);
  runPs(plan.script);
  const result: JsonObject = { ok: true, status: 'ok', action_count: plan.actionCount, display_count: plan.displayCount, all_inputs_released: true };
  if (args.capture_after === true) {
    try {
      const selectedDisplay = numberArg(args, 'capture_display_id', false);
      result.screenshot = desktopScreenshot({
        ...(selectedDisplay === undefined ? {} : { display_id: selectedDisplay }),
        max_width: numberArg(args, 'capture_max_width', false) ?? 1280,
        quality: numberArg(args, 'capture_quality', false) ?? 75
      });
    } catch (error) {
      result.screenshot_error = error instanceof Error ? error.message : String(error);
    }
  }
  return result;
}
