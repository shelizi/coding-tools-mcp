import test from 'node:test';
import assert from 'node:assert/strict';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import { planDesktopInput } from '../dist/desktopInput.js';
import { wrapMcpToolResult } from '../dist/toolContract.js';

test('capture_after returns an MCP image block and a compact screenshot coordinate reference', () => {
  const wrapped = wrapMcpToolResult('desktop_input', { capture_after: true }, {
    ok: true, status: 'ok', action_count: 1,
    screenshot: { mime_type: 'image/jpeg', base64: '/9j/', data_url: 'data:image/jpeg;base64,/9j/',
      image_width: 960, image_height: 540,
      coordinate_reference: { id: 0, name: 'DISPLAY1', x: 0, y: 0, width: 1920, height: 1080, image_width: 960, image_height: 540 } }
  });
  assert.equal(wrapped.content[1].type, 'image');
  assert.equal(wrapped.content[1].mimeType, 'image/jpeg');
  assert.equal(wrapped.content[1].data, '/9j/');
  assert.equal(wrapped.structuredContent.screenshot.base64, undefined);
  assert.equal(wrapped.structuredContent.screenshot.coordinate_reference.image_width, 960);
});


const screens = [
  { id: 0, name: 'DISPLAY1', x: 0, y: 0, width: 3840, height: 2160 },
  { id: 1, name: 'DISPLAY2', x: -2560, y: 180, width: 2560, height: 1440 }
];
const lookup = id => screens[id ?? 0];
const ref1 = { ...screens[1], image_width: 1280, image_height: 720 };
const ref0 = { ...screens[0], image_width: 1920, image_height: 1080 };

test('unified desktop input keeps Ctrl pressed during multiple clicks and releases it', () => {
  const request = {
    actions: [
      { type: 'key_down', keys: ['CTRL', 'SHIFT'] },
      { type: 'click', display_id: 1, x: 500, y: 300, coordinate_reference: ref1 },
      { type: 'click', display_id: 1, x: 700, y: 350, coordinate_reference: ref1 },
      { type: 'key_up', keys: ['CTRL', 'SHIFT'] },
      { type: 'key', keys: ['ALT', 'F4'] },
      { type: 'type', text: '中😀\n' }
    ]
  };
  const plan = planDesktopInput(request, lookup);
  assert.equal(plan.actionCount, 6);
  assert.equal(plan.displayCount, 1);
  assert.match(plan.script, /KeyDown\(17\)/);
  assert.match(plan.script, /KeyDown\(16\)/);
  assert.match(plan.script, /Move\(-1560,780\)/);
  assert.match(plan.script, /Move\(-1159,880\)/);
  assert.match(plan.script, /KeyUp\(16\)/);
  assert.match(plan.script, /KeyUp\(17\)/);
  assert.ok(plan.script.indexOf('KeyDown(17)') < plan.script.indexOf('Move(-1560,780)'));
  assert.ok(plan.script.indexOf('Move(-1159,880)') < plan.script.indexOf('KeyUp(17)'));
  assert.match(plan.script, /Hotkey\(\[uint16\[\]\]@\(18,115\)\)/);
  assert.match(plan.script, /TypeUnicode\(\[Text.Encoding\]::UTF8.GetString/);
  assert.match(plan.script, /heldKeys.Count-1/);
  assert.match(plan.script, /heldButtons.Count-1/);
  assert.match(plan.script, /CodingToolsMCPDesktopInput_v1/);
});

test('Ctrl+Shift drag, cross-display paths, modifier scroll, and button hold/move/up share one input session', () => {
  const plan = planDesktopInput({ actions: [
    { type: 'drag', display_id: 1, x: 500, y: 300, coordinate_reference: ref1,
      path: [{ display_id: 0, x: 1919, y: 1079, coordinate_reference: ref0 }],
      modifiers: ['CTRL', 'SHIFT'], duration_ms: 600, steps: 24 },
    { type: 'scroll', display_id: 1, x: 1279, y: 719, coordinate_reference: ref1,
      delta_y: -120, delta_x: 240, modifiers: ['ALT'] },
    { type: 'mouse_down', button: 'left', display_id: 0, x: 0, y: 0 },
    { type: 'move', display_id: 0, x: 900, y: 700 },
    { type: 'mouse_up', button: 'left' },
    { type: 'wait', duration_ms: 25 }
  ] }, lookup);
  assert.equal(plan.displayCount, 2);
  assert.match(plan.script, /MovePath\(\[int\[\]\]@\(-1560,780,3839,2159\),600,24\)/);
  assert.match(plan.script, /Wheel\(2048,-120\)/);
  assert.match(plan.script, /Wheel\(4096,240\)/);
  assert.match(plan.script, /Move\(900,700\)/);
  assert.match(plan.script, /Sleep\(25\)/);
  assert.equal((plan.script.match(/\[ComputerUseNative\]::MouseDown\(2\)/g) ?? []).length, 2);
  assert.equal((plan.script.match(/\[ComputerUseNative\]::MouseUp\(4\)/g) ?? []).length, 2);
});

test('invalid desktop input batches fail preflight without exposing input state', () => {
  const bad = [
    { actions: [] },
    { actions: [{ type: 'key_up', keys: ['CTRL'] }] },
    { actions: [{ type: 'mouse_up' }] },
    { actions: [{ type: 'key_down', keys: ['CTRL'] }, { type: 'key_down', keys: ['CTRL'] }] },
    { actions: [{ type: 'key_down', keys: ['CTRL'] }, { type: 'key', keys: ['CTRL', 'A'] }] },
    { actions: [{ type: 'click', x: 1920, y: 0, coordinate_reference: ref0 }] },
    { actions: [{ type: 'click', x: 100, y: 100, coordinate_reference: { ...ref1, width: 1 } }] },
    { actions: [{ type: 'drag', x: 1, y: 1, to_x: 2, to_y: 2, steps: 121 }] },
    { actions: [{ type: 'drag', x: 1, y: 1, path: [{ x: 2, y: 3 }], to_x: 10 }] },
    { actions: [{ type: 'scroll', delta_x: 0, delta_y: 0 }] },
    { actions: [{ type: 'wait', duration_ms: 2001 }] },
    { actions: [{ type: 'click', x: 1, y: 1, modifiers: ['BOGUS'] }] },
    { actions: [{ type: 'nonsense' }] }
  ];
  for (const request of bad) {
    assert.throws(() => planDesktopInput(request, lookup), JSON.stringify(request));
  }
});

test('desktop_input executes the full validated sequence via one bridge call, never one per action', { skip: process.platform !== 'win32' }, async () => {
  const childProcess = createRequire(import.meta.url)('node:child_process');
  const saved = childProcess.spawnSync;
  const calls = [];
  childProcess.spawnSync = (_program, _args, options) => {
    const script = String(options?.input ?? '');
    calls.push(script);
    if (script.includes('$selected|ConvertTo-Json -Compress')) {
      return { status: 0, stdout: JSON.stringify(screens[1]), stderr: '' };
    }
    return { status: 0, stdout: '', stderr: '' };
  };
  syncBuiltinESMExports();
  try {
    const { desktopInput } = await import('../dist/desktopInput.js');
    const result = desktopInput({ actions: [
      { type: 'key_down', keys: ['CTRL'] },
      { type: 'click', display_id: 1, x: 500, y: 300, coordinate_reference: ref1 },
      { type: 'key_up', keys: ['CTRL'] }
    ] });
    assert.equal(result.ok, true);
    assert.equal(result.action_count, 3);
    assert.equal(result.all_inputs_released, true);
    assert.equal(calls.filter(s => s.includes('$desktopMutex=')).length, 1);
    const events = calls.find(s => s.includes('$desktopMutex='));
    assert.match(events, /Move\(-1560,780\)/);
    assert.match(events, /KeyDown\(17\)/);
    assert.match(events, /KeyUp\(17\)/);
  } finally {
    childProcess.spawnSync = saved;
    syncBuiltinESMExports();
  }
});
