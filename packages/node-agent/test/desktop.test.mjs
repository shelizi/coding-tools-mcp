import test from 'node:test';
import assert from 'node:assert/strict';
import { desktopToolHandlers } from '../dist/toolDispatchers/desktop.js';
import { toolRuntimeFor } from '../dist/toolRuntime.js';

const desktopToolNames = [
  'desktop_displays',
  'desktop_screenshot',
  'desktop_input'
];

test('desktop tool handlers cover the complete Rust desktop catalog', () => {
  assert.deepEqual(Object.keys(desktopToolHandlers).sort(), [...desktopToolNames].sort());
  for (const name of desktopToolNames) {
    assert.equal(toolRuntimeFor(name).domain, 'desktop', name);
  }
});

test('single desktop input is mutating while display and screenshot are read-only', () => {
  assert.equal(toolRuntimeFor('desktop_displays').mutation, 'never');
  assert.equal(toolRuntimeFor('desktop_screenshot').mutation, 'never');
  assert.equal(toolRuntimeFor('desktop_input').mutation, 'always');
});
