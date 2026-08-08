import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { normalizeConfig } from '../dist/config.js';
import { defaultPolicy, resolveCommandSpec, splitShellWords, validateToolPolicy } from '../dist/policy.js';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';

const nodeProgram = path.basename(process.execPath);

function config(root, dataDir, permissionMode = 'trusted', policy = defaultPolicy()) {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode, policy,
    oauth: { clientId: 'chatgpt', password: 'test-password', tokenSecret: 'test-token-secret' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 }
  };
}

async function fixture(t, permissionMode = 'trusted', policy = defaultPolicy()) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-policy-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-policy-state-'));
  t.after(async () => {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const ctx = await createToolContext(config(root, dataDir, permissionMode, policy));
  const meta = { 'openai/session': `policy-${Date.now()}-${Math.random()}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  return { root, ctx, meta, key: meta['openai/session'] };
}

async function expectPolicyError(ctx, meta, tool, args, code) {
  const result = await callTool(ctx, tool, args, meta);
  assert.equal(result.ok, false, JSON.stringify(result));
  assert.equal(result.error.code, code, JSON.stringify(result));
  return result;
}

test('policy defaults preserve Rust allowlist and configured additions', async () => {
  const defaults = defaultPolicy();
  for (const command of ['git', 'node', 'cargo', 'pwd', 'dir']) assert.ok(defaults.allowedCommands.includes(command));
  assert.equal(defaults.workspaceLocalEntries, true);
  assert.deepEqual(defaults.workspaceScriptExtensions, ['.exe', '.bat', '.cmd', '.ps1']);
  assert.equal(defaults.maxPatchBytes, 200_000);

  const normalized = normalizeConfig({
    schema_version: 1,
    dataDir: path.join(tmpdir(), 'ctmcp-policy-config'),
    folders: [{ id: 'repo', name: 'Repo', path: tmpdir() }],
    policy: { allowedCommands: ['custom-tool'], workspaceScriptExtensions: ['cmd', '.js'], maxPatchBytes: 12345 }
  }, { CTMCP_OAUTH_PASSWORD: 'password', CTMCP_OAUTH_TOKEN_SECRET: 'token' });
  assert.ok(normalized.policy.allowedCommands.includes('git'));
  assert.ok(normalized.policy.allowedCommands.includes('custom-tool'));
  assert.deepEqual(normalized.policy.workspaceScriptExtensions, ['.cmd', '.js']);
  assert.equal(normalized.policy.maxPatchBytes, 12345);
});

test('shell-free cmd uses structured parsing and rejects shell syntax', async t => {
  const { ctx, meta } = await fixture(t);
  assert.deepEqual(splitShellWords('node -e "process.stdout.write(\\"ok\\")"'), [
    'node', '-e', 'process.stdout.write("ok")'
  ]);
  const result = await callTool(ctx, 'exec_command', {
    cmd: `${nodeProgram} -e "process.stdout.write('shell-free')"`,
    yield_time_ms: 30_000,
    output_mode: 'all'
  }, meta);
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.stdout, 'shell-free');

  await expectPolicyError(ctx, meta, 'exec_command', {
    cmd: `${nodeProgram} -e "process.stdout.write('first')" && ${nodeProgram} -e "process.stdout.write('second')"`
  }, 'SHELL_MODE_REQUIRED');
  assert.equal(ctx.sessions.size, 1);
});

test('explicit shell and dangerous commands require confirmation and protect repository assets', async t => {
  const { ctx, meta, key } = await fixture(t);
  await assert.rejects(
    validateToolPolicy(ctx, key, 'exec_command', { script: 'echo ok', shell: 'powershell' }),
    error => error?.code === 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION'
  );
  await assert.rejects(
    validateToolPolicy(ctx, key, 'exec_command', { cmd: 'git reset --hard' }),
    error => error?.code === 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION'
  );
  await assert.rejects(
    validateToolPolicy(ctx, key, 'exec_command', { cmd: 'git clean -fd .git', confirm: true }),
    error => error?.code === 'PROTECTED_REPOSITORY_ASSET'
  );
});

test('allowlist, environment and network policies run before process creation', async t => {
  const guarded = await fixture(t, 'guarded');
  await expectPolicyError(guarded.ctx, guarded.meta, 'exec_command', {
    program: 'definitely-not-allowlisted', args: []
  }, 'COMMAND_REJECTED');
  await expectPolicyError(guarded.ctx, guarded.meta, 'exec_command', {
    program: nodeProgram, args: ['-e', 'process.stdout.write("x")'], env: { PATH: 'blocked' }
  }, 'ENVIRONMENT_VARIABLE_PROTECTED');
  await expectPolicyError(guarded.ctx, guarded.meta, 'exec_command', {
    program: nodeProgram, args: ['-e', 'fetch("https://example.invalid")']
  }, 'NETWORK_COMMAND_BLOCKED');
  assert.equal(guarded.ctx.sessions.size, 0);

  const trusted = await fixture(t, 'trusted');
  await validateToolPolicy(trusted.ctx, trusted.key, 'exec_command', {
    program: nodeProgram, args: ['-e', 'fetch("https://example.invalid")']
  });
});

test('workspace-local executables require an existing in-workspace configured entry', async t => {
  const policy = { ...defaultPolicy(), workspaceScriptExtensions: ['.cmd', '.sh'] };
  const { root, ctx, meta, key } = await fixture(t, 'trusted', policy);
  const directory = path.join(root, 'tools');
  await import('node:fs/promises').then(fs => fs.mkdir(directory, { recursive: true }));
  const relative = process.platform === 'win32' ? 'tools/local-tool.cmd' : 'tools/local-tool.sh';
  const executable = path.join(root, relative);
  await writeFile(executable, process.platform === 'win32' ? '@echo off\r\necho local\r\n' : '#!/bin/sh\necho local\n');
  if (process.platform !== 'win32') await chmod(executable, 0o700);

  await validateToolPolicy(ctx, key, 'exec_command', { program: relative, args: [] });
  const resolved = await resolveCommandSpec(ctx, key, { program: relative, args: [] });
  assert.equal(path.resolve(resolved.program), path.resolve(executable));

  await expectPolicyError(ctx, meta, 'exec_command', {
    program: path.join(root, '..', `outside${path.extname(relative)}`), args: []
  }, 'COMMAND_REJECTED');
});

test('exec_many validates every child before starting any process', async t => {
  const { ctx, meta } = await fixture(t);
  const result = await expectPolicyError(ctx, meta, 'exec_many', {
    mode: 'sequential',
    commands: [
      { id: 'valid', program: nodeProgram, args: ['-e', 'process.stdout.write("should-not-run")'] },
      { id: 'invalid', program: 'not-allowlisted-at-all', args: [] }
    ]
  }, 'COMMAND_REJECTED');
  assert.match(result.error.message, /commands\[1\] rejected/);
  assert.equal(ctx.sessions.size, 0);
});

test('exec_many rejects invalid graph structure before starting any process', async t => {
  const { ctx, meta } = await fixture(t);
  const base = { program: nodeProgram, args: ['-e', 'process.stdout.write("should-not-run")'] };
  const cases = [
    { name: 'duplicate ids', commands: [{ id: 'same', ...base }, { id: 'same', ...base }], message: /duplicate exec_many command id: same/ },
    { name: 'unknown dependency', commands: [{ id: 'known', depends_on: ['missing'], ...base }], message: /depends on unknown command missing/ },
    { name: 'self dependency', commands: [{ id: 'self', depends_on: ['self'], ...base }], message: /cannot depend on itself/ },
    {
      name: 'cycle',
      commands: [{ id: 'left', depends_on: ['right'], ...base }, { id: 'right', depends_on: ['left'], ...base }],
      message: /dependency cycle/
    }
  ];
  for (const item of cases) {
    const result = await expectPolicyError(ctx, meta, 'exec_many', { mode: 'dag', commands: item.commands }, 'INVALID_ARGUMENT');
    assert.match(result.error.message, item.message, item.name);
    assert.equal(ctx.sessions.size, 0, item.name);
  }
});

test('mutation payload limits reject work before files change', async t => {
  const policy = { ...defaultPolicy(), maxPatchBytes: 64 };
  const { root, ctx, meta } = await fixture(t, 'trusted', policy);
  const target = path.join(root, 'target.txt');
  await writeFile(target, 'original\n');
  await expectPolicyError(ctx, meta, 'apply_patch', {
    patch: `*** Begin Patch\n*** Update File: target.txt\n@@\n-original\n+${'x'.repeat(128)}\n*** End Patch`
  }, 'PAYLOAD_TOO_LARGE');
  assert.equal(await import('node:fs/promises').then(fs => fs.readFile(target, 'utf8')), 'original\n');

  await expectPolicyError(ctx, meta, 'edit_file', {
    path: 'target.txt',
    edits: [{ type: 'replace', old_text: 'original', new_text: 'y'.repeat(256) }]
  }, 'PAYLOAD_TOO_LARGE');
  assert.equal(await import('node:fs/promises').then(fs => fs.readFile(target, 'utf8')), 'original\n');
});

test('workdir and timeout stay within configured command bounds', async t => {
  const { ctx, meta } = await fixture(t);
  await expectPolicyError(ctx, meta, 'exec_command', {
    program: nodeProgram, args: ['-e', ''], workdir: '..'
  }, 'PATH_OUTSIDE_WORKSPACE');
  ctx.config.limits.commandTimeoutMaxMs = 1_800_000;
  await expectPolicyError(ctx, meta, 'exec_command', {
    program: nodeProgram, args: ['-e', ''], timeout_ms: 1_800_001
  }, 'INVALID_ARGUMENT');
  assert.equal(ctx.sessions.size, 0);
});
