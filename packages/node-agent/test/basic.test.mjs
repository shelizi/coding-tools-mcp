import test from 'node:test';
import assert from 'node:assert/strict';
import { execFile as execFileCallback } from 'node:child_process';
import { createHash } from 'node:crypto';
import { access, chmod, mkdir, mkdtemp, readFile, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { toolNames, tools } from '../dist/catalog.js';
import { captureGitRestoreSnapshot, restoreGitSnapshot } from '../dist/gitTools.js';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';
import { AGENT_VERSION, CLIENT_COMPAT_VERSION } from '../dist/version.js';

const execFile = promisify(execFileCallback);
const nodeProgram = path.basename(process.execPath);

function config(root, dataDir, permissionMode = 'trusted') {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode,
    oauth: { clientId: 'chatgpt', password: 'test-password', tokenSecret: 'a sufficiently long test token secret' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 1024 * 1024 }
  };
}

async function context(permissionMode = 'trusted') {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-node-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-state-'));
  return { root, ctx: await createToolContext(config(root, dataDir, permissionMode)), meta: { 'openai/session': randomSession() } };
}

function randomSession() { return `session-${Math.random().toString(36).slice(2)}`; }

async function pathExists(value) {
  try { await access(value); return true; } catch { return false; }
}

async function installCustomFormatter(root, source, options = {}) {
  const toolsDir = path.join(root, 'tools');
  const configDir = path.join(root, '.coding-tools');
  await mkdir(toolsDir, { recursive: true });
  await mkdir(configDir, { recursive: true });
  const program = 'tools/custom-formatter.cjs';
  await writeFile(path.join(root, program), source);
  await writeFile(path.join(configDir, 'formatters.json'), `${JSON.stringify({
    formatters: {
      'company-template': {
        program,
        extensions: ['tmpl'],
        args: options.args ?? ['{files}'],
        ...(options.config ? { config: options.config } : {})
      }
    }
  }, null, 2)}\n`);
}

async function select(ctx, meta) {
  const result = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(result.ok, true);
}

async function git(root, ...args) {
  const result = await execFile('git', args, { cwd: root, encoding: 'utf8' });
  return result.stdout.trim();
}

async function gitContext() {
  const state = await context();
  await git(state.root, 'init');
  await git(state.root, 'config', 'user.name', 'Node Agent Test');
  await git(state.root, 'config', 'user.email', 'node-agent@example.invalid');
  await writeFile(path.join(state.root, 'tracked.txt'), 'initial\n');
  await git(state.root, 'add', '--', 'tracked.txt');
  await git(state.root, 'commit', '-m', 'initial');
  await select(state.ctx, state.meta);
  return state;
}

test('catalog exactly matches the Rust P0 tool names', async () => {
  const registryPath = fileURLToPath(new URL('../../../src-tauri/src/tools/registry.rs', import.meta.url));
  const registry = await readFile(registryPath, 'utf8');
  const block = registry.split('pub const P0_TOOLS')[1].split('];')[0];
  const rustNames = [...block.matchAll(/^\s*"([a-z_]+)",\s*$/gm)].map(match => match[1]);
  assert.equal(tools.length, 49);
  assert.deepEqual(toolNames, rustNames);
});

test('server_info reports the package Agent version', async () => {
  const packagePath = fileURLToPath(new URL('../package.json', import.meta.url));
  const packageMetadata = JSON.parse(await readFile(packagePath, 'utf8'));
  const { ctx, meta } = await context();
  const info = await callTool(ctx, 'server_info', {}, meta);
  assert.equal(info.ok, true);
  assert.equal(AGENT_VERSION, packageMetadata.version);
  assert.equal(info.version, packageMetadata.version);
  assert.equal(info.client_compat_version, CLIENT_COMPAT_VERSION);
});

test('workspace selection gates access and read_file returns a bounded slice', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'hello.txt'), 'one\ntwo\nthree\n');
  const denied = await callTool(ctx, 'read_file', { path: 'hello.txt' }, meta);
  assert.equal(denied.ok, false);
  assert.equal(denied.error.code, 'WORKSPACE_FOLDER_NOT_SELECTED');
  await select(ctx, meta);
  const read = await callTool(ctx, 'read_file', { path: 'hello.txt', start_line: 2, end_line: 2 }, meta);
  assert.equal(read.content, 'two\n');
  assert.match(read.sha256, /^[0-9a-f]{64}$/);
});

test('read_many, search_text, list_files and project_map follow the Rust read contracts', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'hello.txt'), 'alpha\nbeta needle\ngamma needle\ndelta\n');
  await writeFile(path.join(root, 'package.json'), JSON.stringify({ scripts: { test: 'node --test', build: 'tsc' } }));
  await import('node:fs/promises').then(({ mkdir }) => mkdir(path.join(root, 'nested')));
  await writeFile(path.join(root, 'nested', 'test.txt'), 'nested needle\n');
  await select(ctx, meta);

  const batch = await callTool(ctx, 'read_many', {
    matches: [
      { path: 'hello.txt', line: 2 },
      { path: 'hello.txt', line: 3 }
    ],
    context_lines: 1,
    line_numbers: true
  }, meta);
  assert.equal(batch.requested_count, 2);
  assert.equal(batch.result_count, 1);
  assert.equal(batch.merged_count, 1);
  assert.deepEqual(batch.results[0].source_indexes, [0, 1]);
  assert.match(batch.results[0].numbered_content, /^\s+1 \| alpha/m);
  assert.equal(batch.results[0].content, 'alpha\nbeta needle\ngamma needle\ndelta\n');

  const firstPage = await callTool(ctx, 'search_text', {
    query: 'needle',
    path: '.',
    include_globs: ['hello.txt'],
    max_results: 1,
    context_lines: 1
  }, meta);
  assert.equal(firstPage.returned_count, 1);
  assert.equal(firstPage.truncated, true);
  assert.equal(firstPage.next_cursor, 1);
  assert.equal(firstPage.matches[0].line, 2);
  assert.deepEqual(firstPage.matches[0].before, ['alpha']);

  const secondPage = await callTool(ctx, 'search_text', {
    query: 'needle',
    path: '.',
    include_globs: ['hello.txt'],
    max_results: 10,
    cursor: firstPage.next_cursor
  }, meta);
  assert.equal(secondPage.matches[0].line, 3);
  assert.equal(secondPage.next_cursor, null);

  const filenameOnly = await callTool(ctx, 'search_text', {
    filename_query: 'nested',
    files_only: true
  }, meta);
  assert.deepEqual(filenameOnly.files.map(item => item.path), ['nested/test.txt']);

  const counted = await callTool(ctx, 'search_text', { query: 'needle', count_only: true }, meta);
  assert.equal(counted.total_matches, 3);
  assert.deepEqual(counted.matches, []);

  const listed = await callTool(ctx, 'list_files', { path: '.', recursive: false }, meta);
  assert.ok(listed.entries.every(entry => entry.type !== 'directory'));
  assert.ok(listed.entries.some(entry => entry.path === 'hello.txt' && typeof entry.size_bytes === 'number'));
  const directories = await callTool(ctx, 'list_files', { path: '.', recursive: false, entry_types: ['directory'] }, meta);
  assert.deepEqual(directories.entries.map(entry => entry.path), ['nested']);

  const project = await callTool(ctx, 'project_map', { max_depth: 3 }, meta);
  assert.ok(project.manifests.some(item => item.path === 'package.json' && item.kind === 'npm'));
  assert.equal(project.package_scripts.test, 'node --test');
  assert.ok(project.suggested_commands.some(item => item.command === 'npm run test'));
});

test('format_files plans safely and applies guarded builtin JSON formatting', async () => {
  const { root, ctx, meta } = await context();
  const file = path.join(root, 'config.json');
  await writeFile(file, '{"b":2,"a":1}');
  await select(ctx, meta);

  const planned = await callTool(ctx, 'format_files', { paths: ['config.json'] }, meta);
  assert.equal(planned.status, 'planned');
  assert.equal(planned.mode, 'plan');
  assert.equal(planned.groups[0].adapter_id, 'builtin-json');
  assert.equal(planned.applied, false);

  const checked = await callTool(ctx, 'format_files', {
    paths: ['config.json'],
    mode: 'check'
  }, meta);
  assert.deepEqual(checked.files_changed, ['config.json']);
  assert.equal(await readFile(file, 'utf8'), '{"b":2,"a":1}');

  const before = await readFile(file);
  const expected = createHash('sha256').update(before).digest('hex');
  const applied = await callTool(ctx, 'format_files', {
    paths: ['config.json'],
    mode: 'apply',
    expected_sha256: { 'config.json': expected }
  }, meta);
  assert.equal(applied.status, 'applied');
  assert.equal(applied.applied, true);
  assert.equal(await readFile(file, 'utf8'), '{\n  "b": 2,\n  "a": 1\n}\n');

  await writeFile(file, '{"stale":true}');
  const conflict = await callTool(ctx, 'format_files', {
    paths: ['config.json'],
    mode: 'apply',
    expected_sha256: { 'config.json': expected }
  }, meta);
  assert.equal(conflict.ok, false);
  assert.equal(conflict.error.code, 'FILE_VERSION_MISMATCH');

  const projectApply = await callTool(ctx, 'format_files', {
    scope: 'project',
    mode: 'apply'
  }, meta);
  assert.equal(projectApply.ok, false);
  assert.equal(projectApply.error.code, 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');
});

test('custom formatter uses workspace configuration and executes only after confirmation', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'page.tmpl'), 'hello\n');
  await installCustomFormatter(root, `
const fs = require('node:fs');
for (const file of process.argv.slice(2)) {
  const value = fs.readFileSync(file, 'utf8');
  fs.writeFileSync(file, value.replaceAll('hello', 'HELLO'));
}
`);
  await select(ctx, meta);

  const planned = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'plan',
    formatter: 'company-template'
  }, meta);
  assert.equal(planned.ok, true);
  assert.equal(planned.custom_formatter_group_count, 1);
  assert.equal(planned.groups[0].adapter_id, 'company-template');
  assert.equal(planned.groups[0].custom, true);
  assert.equal(planned.selection[0].selection_source, 'workspace_config');

  const denied = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'check',
    formatter: 'company-template'
  }, meta);
  assert.equal(denied.ok, false);
  assert.equal(denied.error.code, 'CUSTOM_FORMATTER_REQUIRES_CONFIRMATION');

  const checked = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'check',
    formatter: 'company-template',
    confirm: true
  }, meta);
  assert.equal(checked.ok, true);
  assert.deepEqual(checked.files_changed, ['page.tmpl']);
  assert.equal(checked.applied, false);
  assert.equal(await readFile(path.join(root, 'page.tmpl'), 'utf8'), 'hello\n');

  const applied = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'apply',
    formatter: 'company-template',
    confirm: true
  }, meta);
  assert.equal(applied.ok, true);
  assert.equal(applied.applied, true);
  assert.equal(await readFile(path.join(root, 'page.tmpl'), 'utf8'), 'HELLO\n');
  assert.equal(await pathExists(path.join(root, '.coding-tools-format')), false);
});

test('formatter preserves a pre-existing mirror root directory', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'page.tmpl'), 'hello\n');
  await installCustomFormatter(root, `
const fs = require('node:fs');
for (const file of process.argv.slice(2)) fs.writeFileSync(file, 'HELLO\\n');
`);
  const mirrorParent = path.join(root, '.coding-tools-format');
  await mkdir(mirrorParent);
  await select(ctx, meta);
  const result = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'check',
    formatter: 'company-template',
    confirm: true
  }, meta);
  assert.equal(result.ok, true);
  assert.equal(await pathExists(mirrorParent), true);
  assert.deepEqual(await import('node:fs/promises').then(fs => fs.readdir(mirrorParent)), []);
});

test('custom formatter paths must stay inside the workspace', async () => {
  const { root, ctx, meta } = await context();
  await mkdir(path.join(root, '.coding-tools'), { recursive: true });
  await writeFile(path.join(root, '.coding-tools', 'formatters.json'), JSON.stringify({
    formatters: {
      unsafe: { program: '../formatter.cjs', extensions: ['tmpl'], args: ['{files}'] }
    }
  }));
  await writeFile(path.join(root, 'page.tmpl'), 'hello\n');
  await select(ctx, meta);
  const result = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'plan',
    formatter: 'unsafe'
  }, meta);
  assert.equal(result.ok, false);
  assert.equal(result.error.code, 'FORMATTER_CONFIG_INVALID');
});

test('unexpected formatter changes abort without touching the workspace', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'page.tmpl'), 'hello\n');
  await installCustomFormatter(root, `
const fs = require('node:fs');
for (const file of process.argv.slice(2)) {
  fs.writeFileSync(file, 'FORMATTED\\n');
}
fs.writeFileSync('unexpected.txt', 'surprise\\n');
`);
  await select(ctx, meta);
  const result = await callTool(ctx, 'format_files', {
    paths: ['page.tmpl'],
    mode: 'apply',
    formatter: 'company-template',
    confirm: true
  }, meta);
  assert.equal(result.ok, false);
  assert.equal(result.error.code, 'FORMAT_UNEXPECTED_CHANGES');
  assert.deepEqual(result.error.details.unexpected_changes, ['unexpected.txt']);
  assert.equal(await readFile(path.join(root, 'page.tmpl'), 'utf8'), 'hello\n');
  assert.equal(await pathExists(path.join(root, 'unexpected.txt')), false);
  assert.equal(await pathExists(path.join(root, '.coding-tools-format')), false);
});

test('file_ops performs transactional create, copy, move, delete and mkdir operations', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'source.txt'), 'source\n');
  await writeFile(path.join(root, 'move.txt'), 'move\n');
  await writeFile(path.join(root, 'delete.txt'), 'delete\n');
  await select(ctx, meta);

  const sourceHash = createHash('sha256').update('source\n').digest('hex');
  const dryRun = await callTool(ctx, 'file_ops', {
    dry_run: true,
    operations: [{ type: 'create', path: 'dry-run.txt', content: 'planned\n' }]
  }, meta);
  assert.equal(dryRun.ok, true);
  assert.equal(dryRun.preflight, true);
  assert.equal(dryRun.atomic, true);
  assert.equal(dryRun.applied, false);
  assert.equal(await pathExists(path.join(root, 'dry-run.txt')), false);

  const result = await callTool(ctx, 'file_ops', {
    operations: [
      { type: 'create', path: 'created.txt', content: 'created\n' },
      { type: 'copy', path: 'source.txt', destination: 'nested/copied.txt', expected_sha256: sourceHash },
      { type: 'move', path: 'move.txt', destination: 'moved.txt' },
      { type: 'delete', path: 'delete.txt' },
      { type: 'mkdir', path: 'empty/directory' }
    ]
  }, meta);
  assert.equal(result.ok, true);
  assert.equal(result.atomic, true);
  assert.equal(result.applied, true);
  assert.equal(typeof result.change_id, 'string');
  assert.equal(await readFile(path.join(root, 'created.txt'), 'utf8'), 'created\n');
  assert.equal(await readFile(path.join(root, 'nested', 'copied.txt'), 'utf8'), 'source\n');
  assert.equal(await readFile(path.join(root, 'moved.txt'), 'utf8'), 'move\n');
  assert.equal(await pathExists(path.join(root, 'move.txt')), false);
  assert.equal(await pathExists(path.join(root, 'delete.txt')), false);
  assert.equal(await pathExists(path.join(root, 'empty', 'directory')), true);
});

test('file_ops rolls back staged files when a later directory operation fails', async () => {
  const { root, ctx, meta } = await context();
  await select(ctx, meta);
  const result = await callTool(ctx, 'file_ops', {
    operations: [
      { type: 'create', path: 'nested/collision', content: 'temporary\n' },
      { type: 'mkdir', path: 'nested/collision' }
    ]
  }, meta);
  assert.equal(result.ok, false);
  assert.equal(result.error.code, 'FILE_OPS_APPLY_FAILED');
  assert.deepEqual(result.error.details.rollback_failures, []);
  assert.equal(await pathExists(path.join(root, 'nested', 'collision')), false);
  assert.equal(await pathExists(path.join(root, 'nested')), false);
});

test('file_ops enforces hashes, overwrite confirmation and protected paths', async () => {
  const { root, ctx, meta } = await context();
  await writeFile(path.join(root, 'package.json'), '{}\n');
  await writeFile(path.join(root, 'existing.txt'), 'before\n');
  await select(ctx, meta);

  const stale = await callTool(ctx, 'file_ops', {
    operations: [{
      type: 'delete',
      path: 'existing.txt',
      expected_sha256: '0'.repeat(64)
    }]
  }, meta);
  assert.equal(stale.ok, false);
  assert.equal(stale.error.code, 'FILE_VERSION_MISMATCH');
  assert.equal(await readFile(path.join(root, 'existing.txt'), 'utf8'), 'before\n');

  const overwrite = await callTool(ctx, 'file_ops', {
    operations: [{ type: 'create', path: 'existing.txt', content: 'after\n', overwrite: true }]
  }, meta);
  assert.equal(overwrite.ok, false);
  assert.equal(overwrite.error.code, 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');

  const critical = await callTool(ctx, 'file_ops', {
    operations: [{ type: 'delete', path: 'package.json' }]
  }, meta);
  assert.equal(critical.ok, false);
  assert.equal(critical.error.code, 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');

  const protectedResult = await callTool(ctx, 'file_ops', {
    operations: [{ type: 'create', path: '.git/config', content: 'blocked' }]
  }, meta);
  assert.equal(protectedResult.ok, false);
  assert.equal(protectedResult.error.code, 'PROTECTED_PATH');

  const normalizedProtected = await callTool(ctx, 'file_ops', {
    operations: [{ type: 'create', path: 'safe/../.git/config', content: 'blocked' }]
  }, meta);
  assert.equal(normalizedProtected.ok, false);
  assert.equal(normalizedProtected.error.code, 'PATH_OUTSIDE_WORKSPACE');

  const protectedGithub = await callTool(ctx, 'file_ops', {
    operations: [{ type: 'create', path: '.github/workflows/blocked.yml', content: 'blocked' }]
  }, meta);
  assert.equal(protectedGithub.ok, false);
  assert.equal(protectedGithub.error.code, 'PROTECTED_PATH');
});

test('Git branch and stage support dry-run, expected HEAD and structured status', async () => {
  const { root, ctx, meta } = await gitContext();
  const head = await git(root, 'rev-parse', 'HEAD');
  await writeFile(path.join(root, 'tracked.txt'), 'modified\n');

  const dryStage = await callTool(ctx, 'git_stage', {
    paths: ['tracked.txt'],
    expected_head: head,
    dry_run: true
  }, meta);
  assert.equal(dryStage.ok, true);
  assert.equal(dryStage.applied, false);
  assert.deepEqual(dryStage.command, ['git', 'add', '--', 'tracked.txt']);
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), '');

  const stale = await callTool(ctx, 'git_stage', {
    paths: ['tracked.txt'],
    expected_head: '0'.repeat(40)
  }, meta);
  assert.equal(stale.ok, false);
  assert.equal(stale.error.code, 'EXPECTED_HEAD_MISMATCH');

  const dryBranch = await callTool(ctx, 'git_branch', {
    action: 'create',
    name: 'feature/dry-run',
    switch: false,
    dry_run: true,
    expected_head: head
  }, meta);
  assert.equal(dryBranch.ok, true);
  assert.equal(dryBranch.applied, false);
  assert.equal(await git(root, 'branch', '--list', 'feature/dry-run'), '');

  const current = await git(root, 'branch', '--show-current');
  const created = await callTool(ctx, 'git_branch', {
    action: 'create',
    name: 'feature/no-switch',
    switch: false,
    expected_head: head
  }, meta);
  assert.equal(created.ok, true);
  assert.equal(created.applied, true);
  assert.equal(await git(root, 'branch', '--show-current'), current);
  assert.match(await git(root, 'branch', '--list', 'feature/no-switch'), /feature\/no-switch/);

  const denied = await callTool(ctx, 'git_branch', {
    action: 'delete',
    name: 'feature/no-switch',
    expected_head: head
  }, meta);
  assert.equal(denied.ok, false);
  assert.equal(denied.error.code, 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');
  const deleted = await callTool(ctx, 'git_branch', {
    action: 'delete',
    name: 'feature/no-switch',
    confirm: true,
    expected_head: head
  }, meta);
  assert.equal(deleted.ok, true);
  assert.equal(await git(root, 'branch', '--list', 'feature/no-switch'), '');
});

test('Git mutators route to a selected nested repository and guard its fingerprint', async () => {
  const { root, ctx, meta } = await gitContext();
  const nested = path.join(root, 'nested-repo');
  await mkdir(nested);
  await git(nested, 'init');
  await git(nested, 'config', 'user.email', 'test@example.com');
  await git(nested, 'config', 'user.name', 'Test User');
  await writeFile(path.join(nested, 'tracked.txt'), 'before\n');
  await git(nested, 'add', 'tracked.txt');
  await git(nested, 'commit', '-m', 'initial');
  await writeFile(path.join(nested, 'tracked.txt'), 'after\n');

  const status = await callTool(ctx, 'git_status', { path: 'nested-repo' }, meta);
  assert.equal(status.ok, true);
  assert.match(status.repo_fingerprint, /^[0-9a-f]{64}$/);
  assert.equal(status.repo.repo_path, 'nested-repo');

  const stale = await callTool(ctx, 'git_stage', {
    repo_path: 'nested-repo',
    paths: ['tracked.txt'],
    expected_repo_fingerprint: '0'.repeat(64)
  }, meta);
  assert.equal(stale.ok, false);
  assert.equal(stale.error.code, 'GIT_REPO_TARGET_MISMATCH');

  const staged = await callTool(ctx, 'git_stage', {
    repo_path: 'nested-repo',
    paths: ['tracked.txt'],
    expected_repo_fingerprint: status.repo_fingerprint
  }, meta);
  assert.equal(staged.ok, true);
  assert.equal(staged.repo.repo_path, 'nested-repo');
  assert.equal(await git(nested, 'diff', '--cached', '--name-only'), 'tracked.txt');
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), '');
});

test('git_commit enforces a clean index and returns previous/new HEAD metadata', async () => {
  const { root, ctx, meta } = await gitContext();
  const oldHead = await git(root, 'rev-parse', 'HEAD');
  await writeFile(path.join(root, 'tracked.txt'), 'committed\n');

  const dryRun = await callTool(ctx, 'git_commit', {
    message: 'dry run',
    paths: ['tracked.txt'],
    expected_head: oldHead,
    dry_run: true
  }, meta);
  assert.equal(dryRun.ok, true);
  assert.equal(dryRun.applied, false);
  assert.equal(dryRun.index_clean, true);
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), '');

  const committed = await callTool(ctx, 'git_commit', {
    message: 'commit tracked change',
    paths: ['tracked.txt'],
    expected_head: oldHead
  }, meta);
  assert.equal(committed.ok, true);
  assert.equal(committed.applied, true);
  assert.equal(committed.previous_head, oldHead);
  assert.notEqual(committed.commit, oldHead);
  assert.equal(await git(root, 'rev-parse', 'HEAD'), committed.commit);

  await writeFile(path.join(root, 'pre-staged.txt'), 'staged\n');
  await git(root, 'add', '--', 'pre-staged.txt');
  await writeFile(path.join(root, 'tracked.txt'), 'next\n');
  const blocked = await callTool(ctx, 'git_commit', {
    message: 'must not mix staged changes',
    paths: ['tracked.txt'],
    expected_head: committed.commit
  }, meta);
  assert.equal(blocked.ok, false);
  assert.equal(blocked.error.code, 'GIT_INDEX_NOT_CLEAN');
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), 'pre-staged.txt');
});

test('git_commit restores a clean index after a commit hook failure', async () => {
  const { root, ctx, meta } = await gitContext();
  const head = await git(root, 'rev-parse', 'HEAD');
  await writeFile(path.join(root, 'tracked.txt'), 'hook failure\n');
  const hook = path.join(root, '.git', 'hooks', 'pre-commit');
  await writeFile(hook, '#!/bin/sh\nexit 1\n');
  await chmod(hook, 0o755);

  const failed = await callTool(ctx, 'git_commit', {
    message: 'rejected by hook',
    paths: ['tracked.txt'],
    expected_head: head
  }, meta);
  assert.equal(failed.ok, false);
  assert.equal(failed.error.code, 'GIT_COMMIT_FAILED');
  assert.equal(failed.error.details.staged_by_tool, true);
  assert.equal(failed.error.details.index_restored, true);
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), '');
  assert.equal(await git(root, 'diff', '--name-only'), 'tracked.txt');
  assert.equal(await readFile(path.join(root, 'tracked.txt'), 'utf8'), 'hook failure\n');
});

test('git_restore keeps staged and worktree restoration modes separate', async () => {
  const { root, ctx, meta } = await gitContext();
  const head = await git(root, 'rev-parse', 'HEAD');
  await writeFile(path.join(root, 'tracked.txt'), 'changed\n');
  await git(root, 'add', '--', 'tracked.txt');

  const dryRun = await callTool(ctx, 'git_restore', {
    paths: ['tracked.txt'],
    staged: true,
    confirm: true,
    dry_run: true,
    expected_head: head
  }, meta);
  assert.equal(dryRun.ok, true);
  assert.equal(dryRun.staged, true);
  assert.equal(dryRun.worktree, false);
  assert.equal(dryRun.rollback_protected, true);
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), 'tracked.txt');

  const unstaged = await callTool(ctx, 'git_restore', {
    paths: ['tracked.txt'],
    staged: true,
    confirm: true,
    expected_head: head
  }, meta);
  assert.equal(unstaged.ok, true);
  assert.equal(unstaged.worktree, false);
  assert.equal(unstaged.rollback_protected, true);
  assert.ok(unstaged.snapshot_bytes > 0);
  assert.equal(await git(root, 'diff', '--cached', '--name-only'), '');
  assert.equal(await readFile(path.join(root, 'tracked.txt'), 'utf8'), 'changed\n');

  const restored = await callTool(ctx, 'git_restore', {
    paths: ['tracked.txt'],
    worktree: true,
    confirm: true,
    expected_head: head
  }, meta);
  assert.equal(restored.ok, true);
  assert.equal(restored.rollback_protected, true);
  assert.ok(restored.snapshot_bytes > 0);
  assert.equal((await readFile(path.join(root, 'tracked.txt'), 'utf8')).replaceAll('\r\n', '\n'), 'initial\n');
});

test('git_restore rolls back path-scoped staged and worktree state after Git rejects a mixed path set', async () => {
  const { root, ctx, meta } = await gitContext();
  const key = meta['openai/session'];
  await writeFile(path.join(root, 'deleted.txt'), 'delete-base\n');
  await git(root, 'add', '--', 'deleted.txt');
  await git(root, 'commit', '-m', 'add restore fixture');
  const head = await git(root, 'rev-parse', 'HEAD');

  await writeFile(path.join(root, 'tracked.txt'), 'staged\n');
  await git(root, 'add', '--', 'tracked.txt');
  await writeFile(path.join(root, 'tracked.txt'), 'worktree\n');
  await writeFile(path.join(root, 'added.txt'), 'added\n');
  await git(root, 'add', '--', 'added.txt');
  await git(root, 'rm', '--quiet', '--', 'deleted.txt');
  await writeFile(path.join(root, 'untracked.txt'), 'untracked\n');

  const paths = ['tracked.txt', 'added.txt', 'deleted.txt', 'untracked.txt'];
  const snapshot = await captureGitRestoreSnapshot(ctx, key, paths);
  assert.deepEqual(snapshot.changedPaths, ['added.txt', 'deleted.txt', 'tracked.txt']);
  assert.ok(snapshot.stagedBytes > 0);
  assert.ok(snapshot.worktreeBytes > 0);

  await git(root, 'restore', `--source=${snapshot.head}`, '--staged', '--worktree', '--', ...snapshot.changedPaths);
  const rebuilt = await restoreGitSnapshot(ctx, key, snapshot);
  assert.equal(rebuilt.ok, true);
  assert.deepEqual(rebuilt.steps.map(step => step.step), ['baseline', 'staged_patch', 'worktree_patch']);

  const beforeStatus = await git(root, 'status', '--porcelain=v1');
  const failed = await callTool(ctx, 'git_restore', {
    paths,
    staged: true,
    worktree: true,
    confirm: true,
    expected_head: head
  }, meta);
  assert.equal(failed.ok, false);
  assert.equal(failed.error.code, 'GIT_RESTORE_FAILED');
  assert.equal(failed.error.details.rollback_protected, true);
  assert.equal(failed.error.details.rollback_ok, true);
  assert.ok(failed.error.details.snapshot_bytes > 0);
  assert.equal(await git(root, 'status', '--porcelain=v1'), beforeStatus);
  assert.equal((await readFile(path.join(root, 'tracked.txt'), 'utf8')).replaceAll('\r\n', '\n'), 'worktree\n');
  assert.equal((await readFile(path.join(root, 'added.txt'), 'utf8')).replaceAll('\r\n', '\n'), 'added\n');
  assert.equal(await pathExists(path.join(root, 'deleted.txt')), false);
  assert.equal((await readFile(path.join(root, 'untracked.txt'), 'utf8')).replaceAll('\r\n', '\n'), 'untracked\n');
});

test('guarded mode creates a permission request and resumes the edit', async () => {
  const { root, ctx, meta } = await context('guarded');
  await writeFile(path.join(root, 'hello.txt'), 'before\n');
  await select(ctx, meta);
  const pending = await callTool(ctx, 'edit_file', { path: 'hello.txt', edits: [{ type: 'replace', old_text: 'before', new_text: 'after' }] }, meta);
  assert.equal(pending.ok, false);
  assert.equal(pending.error.code, 'PERMISSION_REQUIRED');
  const resumeId = pending.error.details.permission_request.resume_id;
  const declined = await callTool(ctx, 'request_permissions', { resume_id: resumeId }, meta);
  assert.equal(declined.ok, false);
  assert.equal(declined.error.code, 'PERMISSION_NOT_APPROVED');
  const resumed = await callTool(ctx, 'request_permissions', {
    resume_id: resumeId,
    approve: true,
    confirm: true,
    scope: 'once'
  }, meta);
  assert.equal(resumed.ok, true);
  assert.equal(resumed.resumed, true);
  assert.equal(resumed.permission_grant.status, 'granted_and_resumed');
  assert.equal(await readFile(path.join(root, 'hello.txt'), 'utf8'), 'after\n');
  await ctx.usageStore.flush();
  const telemetry = await ctx.usageStore.query({ scope: 'current_runtime', exclude_tools: [], include_records: true });
  assert.equal(telemetry.records.filter(record => record.tool === 'edit').length, 1, 'internal resume must not be counted as a second MCP request');
});

test('request_permissions requires a resume ID except in dangerous mode', async () => {
  const trusted = await context('trusted');
  await select(trusted.ctx, trusted.meta);
  const unsupported = await callTool(trusted.ctx, 'request_permissions', {
    tool_name: 'exec_command',
    permission: 'inline_script',
    reason: 'test direct grant'
  }, trusted.meta);
  assert.equal(unsupported.ok, false);
  assert.equal(unsupported.status, 'unsupported');
  assert.equal(unsupported.error.code, 'RESUME_ID_REQUIRED');

  const dangerous = await context('dangerous');
  await select(dangerous.ctx, dangerous.meta);
  const granted = await callTool(dangerous.ctx, 'request_permissions', {
    tool_name: 'exec_command',
    permission: 'inline_script',
    reason: 'test dangerous grant'
  }, dangerous.meta);
  assert.equal(granted.ok, true);
  assert.equal(granted.status, 'granted');
  assert.equal(granted.grant_id, 'dangerously-skip-all-permissions');
});

test('exec_many runs a dependency DAG and retains output sessions', async () => {
  const { ctx, meta } = await context();
  await select(ctx, meta);
  const graph = await callTool(ctx, 'exec_many', {
    mode: 'dag', max_parallel: 2,
    commands: [
      { id: 'first', program: nodeProgram, args: ['-e', 'process.stdout.write("first")'] },
      { id: 'second', depends_on: ['first'], program: nodeProgram, args: ['-e', 'process.stdout.write("second")'] }
    ]
  }, meta);
  assert.equal(graph.ok, true);
  assert.equal(graph.failed_command_count, 0);
  assert.equal(graph.results[0].stdout, 'first');
  assert.equal(graph.results[1].stdout, 'second');
  const sessions = await callTool(ctx, 'list_sessions', {}, meta);
  assert.equal(sessions.count, 2);
});

test('retained commands support operation reattachment, post-checks, environment removal and absolute output offsets', async () => {
  const { ctx, meta } = await context();
  await select(ctx, meta);
  process.env.NODE_AGENT_REMOVE_ME = 'secret';
  try {
    const command = {
      operation_id: 'stable-operation',
      program: nodeProgram,
      args: ['-e', 'process.stdout.write(process.env.NODE_AGENT_REMOVE_ME ?? "removed")'],
      remove_env: ['NODE_AGENT_REMOVE_ME'],
      yield_time_ms: 30000,
      output_mode: 'all',
      post_checks: [
        {
          name: 'verify-node',
          program: nodeProgram,
          args: ['-e', 'process.stdout.write("verified")'],
          expected_exit_code: 0
        }
      ]
    };
    const started = await callTool(ctx, 'exec_command', command, meta);
    const finalized = await callTool(ctx, 'wait_command', {
      session_id: started.session_id,
      cursor: started.latest_cursor,
      until: 'finalized',
      timeout_ms: 30000,
      output_mode: 'all'
    }, meta);
    assert.equal(finalized.status, 'exited');
    assert.equal(finalized.command_ok, true);
    assert.equal(finalized.stdout, 'removed');
    assert.equal(finalized.verification_ok, true);
    assert.equal(finalized.post_checks[0].ok, true);
    assert.equal(finalized.post_checks[0].stdout, 'verified');
    assert.equal(finalized.process_tree_contained, process.platform !== 'win32');
    assert.equal(finalized.process_tree_control, process.platform === 'win32' ? 'taskkill_tree' : 'process_group');

    const attached = await callTool(ctx, 'exec_command', command, meta);
    assert.equal(attached.session_id, started.session_id);
    const resolved = await callTool(ctx, 'resolve_operation', {
      operation_id: 'stable-operation',
      output_mode: 'none'
    }, meta);
    assert.equal(resolved.session_id, started.session_id);

    const conflict = await callTool(ctx, 'exec_command', {
      ...command,
      args: ['-e', 'process.stdout.write("different")']
    }, meta);
    assert.equal(conflict.ok, false);
    assert.equal(conflict.error.code, 'OPERATION_ID_CONFLICT');

    const exited = await callTool(ctx, 'list_sessions', { status: 'exited' }, meta);
    assert.ok(exited.sessions.some(session => session.session_id === started.session_id));
    const unfinished = await callTool(ctx, 'list_sessions', { include_finalized: false }, meta);
    assert.ok(unfinished.sessions.every(session => session.finalized_ts_ms === null));
  } finally {
    delete process.env.NODE_AGENT_REMOVE_ME;
  }

  ctx.config.limits.maxOutputBytes = 8;
  const truncated = await callTool(ctx, 'exec_command', {
    program: nodeProgram,
    args: ['-e', 'process.stdout.write("0123456789abcdef")'],
    yield_time_ms: 30000,
    output_mode: 'all'
  }, meta);
  const complete = await callTool(ctx, 'wait_command', {
    session_id: truncated.session_id,
    until: 'finalized',
    timeout_ms: 30000,
    output_mode: 'all'
  }, meta);
  assert.equal(complete.stdout, '89abcdef');
  assert.equal(complete.stdout_retained_from, 8);
  const output = await callTool(ctx, 'read_output', {
    output_ref: complete.output_refs.stdout,
    offset: 0,
    limit: 32
  }, meta);
  assert.equal(output.cursor_expired, true);
  assert.equal(output.offset, 8);
  assert.equal(output.content, '89abcdef');
  assert.equal(output.total_bytes, 16);
});

test('production WSS E2E runner refuses to run without explicit credentials', async () => {
  const script = fileURLToPath(new URL('../scripts/test-production-wss.mjs', import.meta.url));
  const env = Object.fromEntries(Object.entries(process.env).filter(([name]) => !name.startsWith('CTMCP_E2E_')));
  await assert.rejects(
    execFile(process.execPath, [script], { cwd: path.dirname(script), env, encoding: 'utf8' }),
    error => {
      assert.match(String(error.stderr), /CTMCP_E2E_BUILTIN_PUBLIC_URL is required/);
      return true;
    }
  );
});

test('history and durable task state survive tool calls', async () => {
  const { root, ctx, meta } = await context();
  await select(ctx, meta);
  const bootstrap = await callTool(ctx, 'history_session_bootstrap', { session_key: 'task-test' }, meta);
  assert.equal(bootstrap.ok, true);
  const started = await callTool(ctx, 'start_task', { objective: 'Node agent task' }, meta);
  assert.equal(started.task.status, 'active');
  const taskId = started.task.id;
  const updated = await callTool(ctx, 'update_task', {
    task_id: taskId,
    completed_steps: ['Implement'],
    pending_steps: ['Verify']
  }, meta);
  assert.deepEqual(updated.task.completed_steps, ['Implement']);
  const paused = await callTool(ctx, 'pause_task', { task_id: taskId }, meta);
  assert.equal(paused.task.status, 'paused');
  const resumed = await callTool(ctx, 'resume_task', { task_id: taskId }, meta);
  assert.equal(resumed.task.status, 'active');
  const finished = await callTool(ctx, 'finish_task', { task_id: taskId, allow_unverified: true, summary: 'Done' }, meta);
  assert.equal(finished.task.status, 'completed_unverified');
  const taskContext = await callTool(ctx, 'task_context', { task_id: taskId }, meta);
  assert.equal(taskContext.task.id, taskId);
  const events = await callTool(ctx, 'list_task_events', { task_id: taskId, cursor: 0, limit: 20 }, meta);
  assert.ok(events.events.length >= 5);
  const checkpoint = await callTool(ctx, 'history_session_checkpoint', { session_key: 'task-test', expected_path: bootstrap.current_path, tests: ['node test'] }, meta);
  assert.equal(checkpoint.ok, true);
  assert.equal(await readFile(path.join(root, bootstrap.current_path), 'utf8').then(value => value.includes('node test')), true);
});
