import test from 'node:test';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { mkdir, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { discoverExtensions } from '../dist/extensions/discovery.js';
import { ExtensionRegistry } from '../dist/extensions/registry.js';

const hookFixture = fileURLToPath(new URL('./fixtures/hook-extension-fixture.mjs', import.meta.url));
const mcpFixture = fileURLToPath(new URL('./fixtures/mcp-extension-fixture.mjs', import.meta.url));

async function writeJson(file, value) {
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, `${JSON.stringify(value, null, 2)}\n`);
}

test('extension discovery keeps Hook and MCP execution opt-in while supporting Claude and Codex sources', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-extension-root-'));
  const home = await mkdtemp(path.join(tmpdir(), 'ctmcp-extension-home-'));
  const lifecycleLog = path.join(root, 'hook-lifecycle.jsonl');
  let registry;
  t.after(async () => {
    if (registry) await registry.close();
    await Promise.all([
      rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }),
      rm(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 })
    ]);
  });

  await writeJson(path.join(home, '.claude', 'settings.json'), {
    hooks: {
      SessionStart: [{
        matcher: 'startup|resume',
        hooks: [{ type: 'command', command: process.execPath, args: [hookFixture, 'record', lifecycleLog] }]
      }],
      SessionEnd: [{
        hooks: [{ type: 'command', command: process.execPath, args: [hookFixture, 'record', lifecycleLog] }]
      }],
      PreToolUse: [{
        matcher: 'read_file',
        hooks: [{ type: 'command', command: process.execPath, args: [hookFixture, 'rewrite'] }]
      }],
      UnsupportedLifecycleEvent: [{
        hooks: [{ type: 'command', command: process.execPath, args: [hookFixture, 'rewrite'] }]
      }]
    }
  });
  await writeJson(path.join(root, '.codex', 'hooks.json'), {
    hooks: {
      PreToolUse: [{
        matcher: 'write_file',
        hooks: [{ type: 'command', command: process.execPath, args: [hookFixture, 'block'] }]
      }]
    }
  });
  await writeJson(path.join(root, '.mcp.json'), {
    mcpServers: {
      fixture: { type: 'stdio', command: process.execPath, args: [mcpFixture] }
    }
  });
  await mkdir(path.join(home, '.codex'), { recursive: true });
  await writeFile(path.join(home, '.codex', 'config.toml'), [
    '[mcp_servers.codex-http]',
    'url = "https://example.invalid/mcp"',
    'enabled = false',
    ''
  ].join('\n'));

  const folders = [{ id: 'repo', name: 'Repo', path: root }];
  const discovered = await discoverExtensions({ folders, homeDir: home });
  assert.equal(discovered.hooks.some(hook => hook.provider === 'claude' && hook.scope === 'user'), true);
  assert.equal(discovered.hooks.some(hook => hook.provider === 'codex' && hook.scope === 'workspace'), true);
  assert.equal(discovered.hooks.find(hook => hook.event === 'SessionStart')?.supported, true);
  assert.equal(discovered.hooks.find(hook => hook.event === 'SessionEnd')?.supported, true);
  assert.equal(discovered.hooks.find(hook => hook.event === 'UnsupportedLifecycleEvent')?.supported, false);
  const projectMcp = discovered.mcpServers.find(server => server.name === 'fixture');
  assert.ok(projectMcp);
  assert.equal(projectMcp.transport, 'stdio');
  const codexMcp = discovered.mcpServers.find(server => server.name === 'codex-http');
  assert.ok(codexMcp);
  assert.equal(codexMcp.sourceEnabled, false);
  assert.equal(codexMcp.sourcePath, '~/.codex/config.toml');
  assert.doesNotMatch(JSON.stringify(discovered), new RegExp(home.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')));

  registry = new ExtensionRegistry({ folders, homeDir: home });
  const inventory = await registry.inventory(true);
  assert.equal(inventory.hooks.every(item => item.enabled === false), true);
  assert.equal(inventory.mcpServers.every(item => item.enabled === false), true);

  const sessionStartHook = inventory.hooks.find(item => item.hook.event === 'SessionStart');
  const sessionEndHook = inventory.hooks.find(item => item.hook.event === 'SessionEnd');
  assert.ok(sessionStartHook);
  assert.ok(sessionEndHook);
  await registry.setEnabled('hook', [sessionStartHook.hook.key, sessionEndHook.hook.key]);
  await registry.preToolUse('server_info', {}, root, 'lifecycle-session', 'repo');
  await registry.preToolUse('server_info', {}, root, 'lifecycle-session', 'repo');
  await registry.preToolUse('server_info', {}, root, 'lifecycle-session', 'repo');
  await registry.sessionEnd('lifecycle-session', 'repo');
  const lifecycleEvents = (await readFile(lifecycleLog, 'utf8')).trim().split(/\r?\n/).map(line => JSON.parse(line));
  assert.deepEqual(lifecycleEvents.map(event => event.event), ['SessionStart', 'SessionEnd']);
  assert.equal(lifecycleEvents[0].sessionId, 'lifecycle-session');
  assert.equal(lifecycleEvents[0].source, 'startup');

  const rewriteHook = inventory.hooks.find(item => item.hook.provider === 'claude' && item.hook.event === 'PreToolUse');
  assert.ok(rewriteHook);
  await registry.setEnabled('hook', [rewriteHook.hook.key]);
  const rewritten = await registry.preToolUse('read_file', { path: 'hello.txt' }, root, 'fixture-session', 'repo');
  assert.deepEqual(rewritten.input, { path: 'hello.txt', hooked: true });
  assert.deepEqual(rewritten.context, ['hook-extension-context']);

  await registry.setActive('hook', false);
  const hookDisabledInventory = await registry.inventory(true);
  const selectedRewriteHook = hookDisabledInventory.hooks.find(item => item.hook.key === rewriteHook.hook.key);
  assert.equal(selectedRewriteHook.selected, true);
  assert.equal(selectedRewriteHook.enabled, false);
  const unhooked = await registry.preToolUse('read_file', { path: 'hello.txt' }, root, 'fixture-session', 'repo');
  assert.deepEqual(unhooked.input, { path: 'hello.txt' });
  assert.deepEqual(unhooked.context, []);
  await registry.setActive('hook', true);

  const refreshed = await registry.inventory(true);
  const blockHook = refreshed.hooks.find(item => item.hook.provider === 'codex' && item.hook.event === 'PreToolUse');
  assert.ok(blockHook);
  await registry.setEnabled('hook', [blockHook.hook.key]);
  const blocked = await registry.preToolUse('write_file', { path: 'blocked.txt' }, root, 'fixture-session', 'repo');
  assert.equal(blocked.blocked?.message, 'blocked-by-extension-fixture');

  const mcpInventory = await registry.inventory(true);
  const fixtureServer = mcpInventory.mcpServers.find(item => item.server.name === 'fixture');
  assert.ok(fixtureServer);
  await registry.setEnabled('mcp', [fixtureServer.server.key]);
  const definitions = registry.toolDefinitions();
  const echo = definitions.find(tool => tool.name.includes('__fixture__echo'));
  assert.ok(echo);
  const result = await registry.callExternalTool(echo.name, { message: 'hello' }, root, 'fixture-session');
  assert.equal(result.content[0].text, 'fixture:hello');
  assert.deepEqual(result.structuredContent, { echoed: 'hello' });

  await registry.setActive('mcp', false);
  const mcpDisabledInventory = await registry.inventory(true);
  const selectedFixture = mcpDisabledInventory.mcpServers.find(item => item.server.key === fixtureServer.server.key);
  assert.equal(selectedFixture.selected, true);
  assert.equal(selectedFixture.enabled, false);
  assert.deepEqual(registry.toolDefinitions(), []);
  await registry.setActive('mcp', true);
  assert.ok(registry.toolDefinitions().some(tool => tool.name.includes('__fixture__echo')));
});

test('external MCP stdio supports Windows batch launchers', { skip: process.platform !== 'win32' }, async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-extension-bat-root-'));
  const home = await mkdtemp(path.join(tmpdir(), 'ctmcp-extension-bat-home-'));
  let registry;
  t.after(async () => {
    if (registry) await registry.close();
    await Promise.all([
      rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }),
      rm(home, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 })
    ]);
  });

  const launcher = path.join(root, 'fixture-mcp.bat');
  await writeFile(launcher, `@echo off\r\n"${process.execPath}" %*\r\n`);
  await writeJson(path.join(root, '.mcp.json'), {
    mcpServers: {
      'batch-fixture': { type: 'stdio', command: launcher, args: [mcpFixture] }
    }
  });

  const folders = [{ id: 'repo', name: 'Repo', path: root }];
  registry = new ExtensionRegistry({ folders, homeDir: home });
  const inventory = await registry.inventory(true);
  const batchServer = inventory.mcpServers.find(item => item.server.name === 'batch-fixture');
  assert.ok(batchServer);
  await registry.setEnabled('mcp', [batchServer.server.key]);
  const echo = registry.toolDefinitions().find(tool => tool.name.includes('__batch-fixture__echo'));
  assert.ok(echo);
  const result = await registry.callExternalTool(echo.name, { message: 'batch' }, root, 'fixture-session');
  assert.equal(result.content[0].text, 'fixture:batch');
});
