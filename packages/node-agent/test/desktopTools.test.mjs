import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createRequire, syncBuiltinESMExports } from 'node:module';
import { desktopKeyCode, desktopScrollScript, desktopTypePayload, mapDisplayPoint, planScreenshotSize } from '../dist/desktopTools.js';

const require = createRequire(import.meta.url);

test('desktop coordinate and text planning is side-effect free', () => {
  assert.deepEqual(
    mapDisplayPoint({ id: 1, x: -2560, y: 180, width: 2560, height: 1440 }, 125, 75),
    { x: 125, y: 75, gx: -2435, gy: 255 }
  );
  assert.deepEqual(
    mapDisplayPoint({ id: 2, x: 1920, y: -1200, width: 1600, height: 1200 }, 10, 20),
    { x: 10, y: 20, gx: 1930, gy: -1180 }
  );

  const display = { id: 0, x: 0, y: 0, width: 1920, height: 1080 };
  for (const [x, y] of [[-1, 0], [0, -1], [1920, 0], [0, 1080], [1.5, 2]]) {
    assert.throws(() => mapDisplayPoint(display, x, y));
  }

  const text = `A中😀\nquote:'\" & | < >`;
  const payload = desktopTypePayload(text);
  assert.equal(Buffer.from(payload.base64, 'base64').toString('utf8'), text);
  assert.equal(payload.typedUtf16Units, text.length);
  assert.equal(desktopTypePayload('😀').typedUtf16Units, 2);

  assert.equal(desktopKeyCode('F1'), 0x70);
  assert.equal(desktopKeyCode('f12'), 0x7b);
  assert.equal(desktopKeyCode('F24'), 0x87);
  assert.equal(desktopKeyCode('F25'), undefined);

  const scrollScript = desktopScrollScript('', -240, -1200);
  assert.match(scrollScript, /Wheel\(2048,-1200\)/);
  assert.match(scrollScript, /Wheel\(4096,-240\)/);
  assert.equal(scrollScript.includes('[uint32]'), false);
});

test('screenshot scaling maps image pixels and rejects a stale display configuration', () => {
  assert.deepEqual(planScreenshotSize(3840, 2160), { width: 3840, height: 2160 });
  assert.deepEqual(planScreenshotSize(3840, 2160, 1920), { width: 1920, height: 1080 });
  assert.deepEqual(planScreenshotSize(3840, 2160, 1920, 720), { width: 1280, height: 720 });
  assert.deepEqual(planScreenshotSize(1024, 768, 4096, 4096), { width: 1024, height: 768 });
  for (const width of [0, -1, 16385, 1.5, NaN]) {
    assert.throws(() => planScreenshotSize(1920, 1080, width));
  }
  const display = { id: 1, name: 'DISPLAY2', x: -2560, y: 180, width: 2560, height: 1440 };
  const reference = { ...display, image_width: 1280, image_height: 720 };
  assert.deepEqual(mapDisplayPoint(display, 500, 300, reference),
    { x: 1000, y: 600, gx: -1560, gy: 780 });
  assert.deepEqual(mapDisplayPoint(display, 1279, 719, reference),
    { x: 2559, y: 1439, gx: -1, gy: 1619 });
  assert.deepEqual(mapDisplayPoint(display, 0, 0, { ...display, image_width: 1, image_height: 1 }),
    { x: 1280, y: 720, gx: -1280, gy: 900 });
  assert.throws(() => mapDisplayPoint(display, 1280, 0, reference), /outside screenshot bounds/);
  assert.throws(() => mapDisplayPoint(display, 10, 10, { ...reference, width: 1920 }), /Display changed since screenshot/);
  assert.throws(() => mapDisplayPoint(display, 10, 10, { ...reference, x: 0 }), /Display changed since screenshot/);
  assert.throws(() => mapDisplayPoint(display, 10, 10, { ...reference, name: 'DISPLAY3' }), /Display changed since screenshot/);
  assert.throws(() => mapDisplayPoint(display, 10, 10, { ...reference, image_width: 0 }), /coordinate_reference/);
});

test('desktop click/drag/type/key production bridge can be verified without real input', { skip: process.platform !== 'win32' }, async () => {
  const childProcess = require('node:child_process');
  const originalSpawnSync = childProcess.spawnSync;
  const calls = [];

  childProcess.spawnSync = (program, args, options = {}) => {
    const script = String(options.input ?? '');
    calls.push({ program, args, script });
    if (script.includes('$selected|ConvertTo-Json -Compress')) {
      return {
        status: 0,
        stdout: JSON.stringify({ id: 1, name: 'DISPLAY2', x: -2560, y: 180, width: 2560, height: 1440, primary: false }),
        stderr: ''
      };
    }
    return { status: 0, stdout: script.includes('CopyFromScreen') ? '/9j/' : '', stderr: '' };
  };
  syncBuiltinESMExports();

  try {
    const source = await readFile(new URL('../dist/desktopTools.js', import.meta.url), 'utf8');
    const moduleUrl = `data:text/javascript;base64,${Buffer.from(source).toString('base64')}`;
    const desktop = await import(moduleUrl);

    const screenshot = desktop.desktopScreenshot({ display_id: 1, quality: 100 });
    assert.equal(screenshot.quality, 100);
    assert.equal(screenshot.bytes, 3);
    assert.equal(screenshot.resized, false);
    assert.equal(screenshot.image_width, 2560);
    assert.equal(screenshot.image_height, 1440);
    assert.deepEqual(screenshot.coordinate_reference, {
      id: 1, name: 'DISPLAY2', x: -2560, y: 180,
      width: 2560, height: 1440, image_width: 2560, image_height: 1440
    });
    const resized = desktop.desktopScreenshot({ display_id: 1, max_width: 1280, max_height: 900 });
    assert.equal(resized.width, 2560);
    assert.equal(resized.image_width, 1280);
    assert.equal(resized.image_height, 720);
    assert.equal(resized.resized, true);
    assert.match(calls.at(-1).script, /DrawImage\(\$bmp,0,0,\$iw,\$ih\)/);
    const screenshotBridgeCalls = calls.filter(call => call.script.includes('CopyFromScreen')).length;
    assert.throws(() => desktop.desktopScreenshot({ display_id: 1, quality: 0 }), /quality must be between 1 and 100/);
    assert.throws(() => desktop.desktopScreenshot({ display_id: 1, quality: 101 }), /quality must be between 1 and 100/);
    assert.equal(calls.filter(call => call.script.includes('CopyFromScreen')).length, screenshotBridgeCalls);

    const click = desktop.desktopClick({ display_id: 1, x: 125, y: 75, button: 'right', clicks: 2 });
    assert.equal(click.global_x, -2435);
    assert.equal(click.global_y, 255);
    assert.equal(click.x, 125);
    assert.equal(click.y, 75);
    const clickScript = calls.at(-1).script;
    assert.match(clickScript, /\[void\]\[ComputerUseNative\]::SetCursorPos\(-2435,255\);1\.\.2/);
    assert.match(clickScript, /mouse_event\(8,0,0,0/);
    assert.match(clickScript, /mouse_event\(16,0,0,0/);
    const resizedClick = desktop.desktopClick({
      display_id: 1, x: 500, y: 300, coordinate_reference: resized.coordinate_reference
    });
    assert.equal(resizedClick.global_x, -1560);
    assert.equal(resizedClick.global_y, 780);
    assert.equal(resizedClick.input_x, 500);
    assert.equal(resizedClick.x, 1000);
    assert.match(calls.at(-1).script, /SetCursorPos\(-1560,780\)/);
    const inputBeforeStale = calls.filter(call => call.script.includes('[void][ComputerUseNative]::SetCursorPos(')).length;
    assert.throws(() => desktop.desktopClick({
      display_id: 1, x: 500, y: 300, coordinate_reference: { ...resized.coordinate_reference, height: 1080 }
    }), /Display changed since screenshot/);
    assert.equal(calls.filter(call => call.script.includes('[void][ComputerUseNative]::SetCursorPos(')).length, inputBeforeStale);

    const tripleClick = desktop.desktopClick({ display_id: 1, x: 0, y: 0, clicks: 3 });
    assert.equal(tripleClick.clicks, 3);
    assert.match(calls.at(-1).script, /1\.\.3/);
    const clickBridgeCallsBeforeClickLimit = calls.filter(call => call.script.includes('[void][ComputerUseNative]::SetCursorPos(')).length;
    assert.throws(() => desktop.desktopClick({ display_id: 1, x: 0, y: 0, clicks: 4 }), /clicks must be between 1 and 3/);
    assert.equal(
      calls.filter(call => call.script.includes('[void][ComputerUseNative]::SetCursorPos(')).length,
      clickBridgeCallsBeforeClickLimit
    );

    const inputBridgeCallsBeforeBoundsCheck = calls.filter(call => call.script.includes('[void][ComputerUseNative]::SetCursorPos(')).length;
    assert.throws(() => desktop.desktopClick({ display_id: 1, x: 2560, y: 0 }), /outside display bounds/);
    const inputBridgeCallsAfterBoundsCheck = calls.filter(call => call.script.includes('[void][ComputerUseNative]::SetCursorPos(')).length;
    assert.equal(inputBridgeCallsAfterBoundsCheck, inputBridgeCallsBeforeBoundsCheck);

    const drag = desktop.desktopDrag({
      display_id: 1, x: 125, y: 75,
      to_display_id: 1, to_x: 800, to_y: 500,
      button: 'left', duration_ms: 450, steps: 18
    });
    assert.equal(drag.global_x, -2435);
    assert.equal(drag.global_y, 255);
    assert.equal(drag.to_global_x, -1760);
    assert.equal(drag.to_global_y, 680);
    assert.equal(drag.duration_ms, 450);
    assert.equal(drag.steps, 18);
    const dragScript = calls.at(-1).script;
    assert.match(dragScript, /Drag\(-2435,255,-1760,680,2,4,450,18\)/);
    const imageDrag = desktop.desktopDrag({
      display_id: 1, x: 500, y: 300, to_x: 1279, to_y: 719,
      coordinate_reference: resized.coordinate_reference
    });
    assert.equal(imageDrag.global_x, -1560);
    assert.equal(imageDrag.to_global_x, -1);
    assert.equal(imageDrag.to_global_y, 1619);
    const dragBridgeCalls = calls.filter(call => call.script.includes('[ComputerUseNative]::Drag(')).length;
    assert.throws(() => desktop.desktopDrag({ display_id: 1, x: 1, y: 1, to_x: 2, to_y: 2, duration_ms: 5001 }), /duration_ms must be between 0 and 5000/);
    assert.throws(() => desktop.desktopDrag({ display_id: 1, x: 1, y: 1, to_x: 2, to_y: 2, steps: 121 }), /steps must be between 1 and 120/);
    assert.equal(calls.filter(call => call.script.includes('[ComputerUseNative]::Drag(')).length, dragBridgeCalls);

    const text = `A中😀\nquote:'\" & | < >`;
    const typed = desktop.desktopType({ text });
    assert.equal(typed.typed_utf16_units, text.length);
    const typeScript = calls.at(-1).script;
    const encoded = typeScript.match(/FromBase64String\('([^']+)'\)/)?.[1];
    assert.ok(encoded);
    assert.equal(Buffer.from(encoded, 'base64').toString('utf8'), text);
    assert.equal(typeScript.includes(text), false);
    assert.match(typeScript, /dwFlags=4/);
    assert.match(typeScript, /dwFlags=6/);
    assert.match(typeScript, /struct MOUSEINPUT/);
    assert.match(typeScript, /struct HARDWAREINPUT/);
    assert.match(typeScript, /IntPtr\.Size==8\?40:28/);
    assert.match(typeScript, /SendInput\(\(uint\)inputs\.Length,inputs,InputSize\(\)\)/);

    const hotkey = desktop.desktopKey({ keys: ['CTRL', 'A'] });
    assert.deepEqual(hotkey.keys, ['CTRL', 'A']);
    const hotkeyScript = calls.at(-1).script;
    assert.match(hotkeyScript, /Hotkey\(\[uint16\[\]\]@\(17,65\)\)/);

    const functionHotkey = desktop.desktopKey({ keys: ['ALT', 'F4'] });
    assert.deepEqual(functionHotkey.keys, ['ALT', 'F4']);
    assert.match(calls.at(-1).script, /Hotkey\(\[uint16\[\]\]@\(18,115\)\)/);

    const scroll = desktop.desktopScroll({ display_id: 1, x: 125, y: 75, delta_x: -240, delta_y: -1200 });
    assert.equal(scroll.delta_x, -240);
    assert.equal(scroll.delta_y, -1200);
    const scrollBridgeScript = calls.at(-1).script;
    assert.match(scrollBridgeScript, /SetCursorPos\(-2435,255\)/);
    assert.match(scrollBridgeScript, /Wheel\(2048,-1200\)/);
    assert.match(scrollBridgeScript, /Wheel\(4096,-240\)/);
    assert.equal(scrollBridgeScript.includes('[uint32]([int32]-1200)'), false);
    const scrollFromImage = desktop.desktopScroll({
      display_id: 1, x: 500, y: 300,
      coordinate_reference: resized.coordinate_reference, delta_y: -120
    });
    assert.equal(scrollFromImage.delta_y, -120);
    assert.match(calls.at(-1).script, /SetCursorPos\(-1560,780\)/);

    const eightKeyChord = ['CTRL', 'SHIFT', 'ALT', 'WIN', 'A', 'B', 'C', 'D'];
    assert.deepEqual(desktop.desktopKey({ keys: eightKeyChord }).keys, eightKeyChord);
    const hotkeyBridgeCalls = calls.filter(call => call.script.includes('[ComputerUseNative]::Hotkey(')).length;
    assert.throws(
      () => desktop.desktopKey({ keys: [...eightKeyChord, 'E'] }),
      /keys must contain 1 to 8 strings/
    );
    assert.equal(calls.filter(call => call.script.includes('[ComputerUseNative]::Hotkey(')).length, hotkeyBridgeCalls);

    assert.ok(calls.length >= 5);
  } finally {
    childProcess.spawnSync = originalSpawnSync;
    syncBuiltinESMExports();
  }
});
