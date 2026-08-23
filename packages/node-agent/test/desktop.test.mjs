import test from 'node:test';
import assert from 'node:assert/strict';
import { desktopToolHandlers } from '../dist/toolDispatchers/desktop.js';
import { toolRuntimeFor } from '../dist/toolRuntime.js';

const desktopToolNames = [
  'desktop_displays',
  'desktop_screenshot',
  'desktop_click',
  'desktop_drag',
  'desktop_scroll',
  'desktop_type',
  'desktop_key'
];

test('desktop tool handlers cover the complete Rust desktop catalog', () => {
  assert.deepEqual(Object.keys(desktopToolHandlers).sort(), [...desktopToolNames].sort());
  for (const name of desktopToolNames) {
    assert.equal(toolRuntimeFor(name).domain, 'desktop', name);
  }
});

test('desktop input tools remain mutating while display and screenshot are read-only', () => {
  assert.equal(toolRuntimeFor('desktop_displays').mutation, 'never');
  assert.equal(toolRuntimeFor('desktop_screenshot').mutation, 'never');
  for (const name of ['desktop_click', 'desktop_drag', 'desktop_scroll', 'desktop_type', 'desktop_key']) {
    assert.equal(toolRuntimeFor(name).mutation, 'always', name);
  }
});
