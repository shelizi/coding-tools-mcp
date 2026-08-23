import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';

const read = relative => readFile(new URL(`../${relative}`, import.meta.url), 'utf8');

test('Node Agent dev workflow builds before restart and health-checks the replacement', async () => {
  const [pkg, watcher, launcher] = await Promise.all([
    read('packages/node-agent/package.json'),
    read('packages/node-agent/scripts/dev-server.mjs'),
    read('packages/node-agent/scripts/dev-server.ps1')
  ]);
  const scripts = JSON.parse(pkg).scripts;
  assert.match(scripts['dev:server'], /dev-server\.ps1/);
  assert.match(scripts['dev:server:stop'], /-Stop/);
  assert.match(scripts['dev:server:once'], /-Once -BuildOnly/);
  assert.ok(watcher.indexOf('await buildServer()') < watcher.indexOf('await stopCurrentAgent(endpoints)'));
  assert.match(watcher, /body\?\.server === 'coding-tools-mcp-node'/);
  assert.match(watcher, /keeping the current Agent running/);
  assert.match(watcher, /watch\(target, \{ recursive \}/);
  assert.match(launcher, /Invoke-CimMethod -ClassName Win32_Process -MethodName Create/);
});

test('Rust fast dev workflow enables incremental compilation and delegates watch-restart to Tauri', async () => {
  const [pkg, script, cargo, cmd] = await Promise.all([
    read('package.json'),
    read('scripts/dev-rust-desktop.ps1'),
    read('src-tauri/Cargo.toml'),
    read('dev-desktop.cmd')
  ]);
  const scripts = JSON.parse(pkg).scripts;
  assert.match(scripts['desktop:dev:fast'], /dev-rust-desktop\.ps1/);
  assert.match(scripts['rust:dev:build'], /-OnceBuild/);
  assert.match(script, /CARGO_INCREMENTAL = '1'/);
  assert.match(script, /CARGO_TARGET_DIR/);
  assert.match(script, /& \$tauri dev @TauriArgs/);
  assert.match(cargo, /\[profile\.dev\][\s\S]*incremental = true/);
  assert.match(cmd, /pnpm run desktop:dev:fast/);
});
