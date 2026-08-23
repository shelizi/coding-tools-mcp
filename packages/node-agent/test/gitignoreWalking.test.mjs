import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { isIgnoredByRules, parseIgnoreFile } from '../dist/gitignore.js';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';

function config(root, dataDir) {
  return {
    host: '127.0.0.1',
    port: 0,
    dataDir,
    permissionMode: 'trusted',
    management: { enabled: false },
    oauth: {
      clientId: 'chatgpt',
      password: 'gitignore-test-password',
      tokenSecret: 'gitignore-walking-token-secret'
    },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: {
      blockingConcurrency: 4,
      processConcurrency: 4,
      activeSessionLimit: 16,
      maxOutputBytes: 1024 * 1024
    }
  };
}

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-ignore-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-ignore-data-'));
  t.after(async () => {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const ctx = await createToolContext(config(root, dataDir));
  const meta = { 'openai/session': `ignore-${Date.now()}-${Math.random()}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  return { root, ctx, meta };
}

async function put(root, relative, content = 'needle\n') {
  const file = path.join(root, relative);
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, content);
}

async function listedPaths(ctx, meta, args = {}) {
  const result = await callTool(ctx, 'list_files', { path: '.', max_results: 500, ...args }, meta);
  assert.equal(result.ok, true, JSON.stringify(result));
  return result.entries.map(entry => entry.path);
}

test('gitignore parser supports comments, escaped prefixes, globstar and classes', () => {
  const rules = parseIgnoreFile([
    '# comment',
    String.raw`\#literal`,
    String.raw`\!literal`,
    'logs/**/debug?.[ch]',
    '*.log',
    '!keep.log',
    'cache/*',
    '!cache/keep.txt',
    '/root-only.txt'
  ].join('\n'), '');
  assert.equal(isIgnoredByRules('#literal', false, rules), true);
  assert.equal(isIgnoredByRules('!literal', false, rules), true);
  assert.equal(isIgnoredByRules('logs/debug1.c', false, rules), true);
  assert.equal(isIgnoredByRules('logs/a/b/debug2.h', false, rules), true);
  assert.equal(isIgnoredByRules('logs/a/b/debug20.h', false, rules), false);
  assert.equal(isIgnoredByRules('drop.log', false, rules), true);
  assert.equal(isIgnoredByRules('nested/drop.log', false, rules), true);
  assert.equal(isIgnoredByRules('keep.log', false, rules), false);
  assert.equal(isIgnoredByRules('cache/drop.txt', false, rules), true);
  assert.equal(isIgnoredByRules('cache/keep.txt', false, rules), false);
  assert.equal(isIgnoredByRules('root-only.txt', false, rules), true);
  assert.equal(isIgnoredByRules('nested/root-only.txt', false, rules), false);
});

test('list and search respect root, nested and negated gitignore rules', async t => {
  const { root, ctx, meta } = await fixture(t);
  await put(root, '.gitignore', [
    'ignored/',
    '*.log',
    '!keep.log',
    'cache/*',
    '!cache/keep.txt',
    'restored/',
    '!restored/',
    'restored/*',
    '!restored/keep.txt',
    '/root-only.txt'
  ].join('\n'));
  await put(root, 'src/.gitignore', [
    'generated/',
    '*.tmp',
    '!important.tmp'
  ].join('\n'));
  for (const file of [
    'visible.txt', 'drop.log', 'keep.log', 'root-only.txt', 'nested/root-only.txt',
    'ignored/hidden.txt', 'cache/drop.txt', 'cache/keep.txt',
    'restored/drop.txt', 'restored/keep.txt', 'src/main.ts', 'src/drop.tmp',
    'src/important.tmp', 'src/generated/output.ts'
  ]) await put(root, file);

  const paths = await listedPaths(ctx, meta);
  assert.deepEqual(paths, [
    'cache/keep.txt',
    'keep.log',
    'nested/root-only.txt',
    'restored/keep.txt',
    'src/important.tmp',
    'src/main.ts',
    'visible.txt'
  ]);

  const nested = await callTool(ctx, 'list_files', { path: 'src', max_results: 100 }, meta);
  assert.equal(nested.ok, true, JSON.stringify(nested));
  assert.deepEqual(nested.entries.map(entry => entry.path), ['src/important.tmp', 'src/main.ts']);

  const depthOne = await callTool(ctx, 'list_files', {
    path: '.',
    recursive: true,
    max_depth: 1,
    max_results: 100,
    entry_types: ['file', 'directory']
  }, meta);
  assert.equal(depthOne.ok, true, JSON.stringify(depthOne));
  assert.deepEqual(depthOne.entries.map(entry => entry.path), [
    'cache', 'keep.log', 'nested', 'restored', 'src', 'visible.txt'
  ]);

  const depthTwo = await callTool(ctx, 'list_files', {
    path: '.',
    recursive: true,
    max_depth: 2,
    max_results: 100,
    entry_types: ['file', 'directory']
  }, meta);
  assert.equal(depthTwo.ok, true, JSON.stringify(depthTwo));
  assert.ok(depthTwo.entries.some(entry => entry.path === 'cache/keep.txt'));
  assert.ok(depthTwo.entries.some(entry => entry.path === 'nested/root-only.txt'));
  assert.ok(depthTwo.entries.some(entry => entry.path === 'restored/keep.txt'));
  assert.ok(depthTwo.entries.some(entry => entry.path === 'src/main.ts'));

  const search = await callTool(ctx, 'search_text', { query: 'needle', max_results: 100 }, meta);
  assert.equal(search.ok, true, JSON.stringify(search));
  assert.deepEqual(search.matches.map(match => match.path), [
    'cache/keep.txt',
    'keep.log',
    'nested/root-only.txt',
    'restored/keep.txt',
    'src/important.tmp',
    'src/main.ts',
    'visible.txt'
  ]);
});

test('ignored, hidden, and generated traversal are independent while .git is always excluded', async t => {
  const { root, ctx, meta } = await fixture(t);
  await put(root, '.gitignore', 'ignored.txt\n');
  await put(root, '.ignore', 'ignore-file.txt\n');
  await put(root, '.git/info/exclude', 'info-ignored.txt\n');
  for (const file of [
    'visible.txt', 'ignored.txt', 'ignore-file.txt', 'info-ignored.txt',
    '.hidden.txt', '..double-dot.txt', '.hidden-dir/inside.txt',
    'node_modules/pkg.txt', '.git/config', '.GIT/upper.txt'
  ]) await put(root, file);

  const defaults = await listedPaths(ctx, meta);
  assert.deepEqual(defaults, ['visible.txt']);

  const ignored = await listedPaths(ctx, meta, { include_ignored: true });
  assert.deepEqual(ignored, ['ignore-file.txt', 'ignored.txt', 'info-ignored.txt', 'visible.txt']);
  const generated = await listedPaths(ctx, meta, { include_ignored: true, include_generated: true });
  assert.deepEqual(generated, ['ignore-file.txt', 'ignored.txt', 'info-ignored.txt', 'node_modules/pkg.txt', 'visible.txt']);

  const hidden = await listedPaths(ctx, meta, { include_hidden: true });
  assert.ok(hidden.includes('.gitignore'));
  assert.ok(hidden.includes('.ignore'));
  assert.ok(hidden.includes('.hidden.txt'));
  assert.ok(hidden.includes('..double-dot.txt'));
  assert.ok(hidden.includes('.hidden-dir/inside.txt'));
  assert.ok(hidden.includes('visible.txt'));
  assert.ok(!hidden.includes('ignored.txt'));
  assert.ok(!hidden.includes('ignore-file.txt'));
  assert.ok(!hidden.includes('info-ignored.txt'));
  assert.ok(!hidden.includes('node_modules/pkg.txt'));
  assert.ok(!hidden.some(value => value.toLowerCase().startsWith('.git/')));

  const all = await listedPaths(ctx, meta, { include_hidden: true, include_ignored: true });
  assert.ok(all.includes('.gitignore'));
  assert.ok(all.includes('.ignore'));
  assert.ok(all.includes('.hidden.txt'));
  assert.ok(all.includes('..double-dot.txt'));
  assert.ok(all.includes('.hidden-dir/inside.txt'));
  assert.ok(all.includes('ignored.txt'));
  assert.ok(all.includes('ignore-file.txt'));
  assert.ok(all.includes('info-ignored.txt'));
  assert.ok(!all.includes('node_modules/pkg.txt'));
  assert.ok(!all.some(value => value.toLowerCase().startsWith('.git/')));

  const everything = await listedPaths(ctx, meta, { include_hidden: true, include_ignored: true, include_generated: true });
  assert.ok(everything.includes('node_modules/pkg.txt'));
  assert.ok(!everything.some(value => value.toLowerCase().startsWith('.git/')));

  const directGit = await callTool(ctx, 'list_files', {
    path: '.git',
    include_hidden: true,
    include_ignored: true,
    max_results: 100
  }, meta);
  assert.equal(directGit.ok, true, JSON.stringify(directGit));
  assert.deepEqual(directGit.entries, []);

  const directHiddenBlocked = await callTool(ctx, 'list_files', {
    path: '.hidden-dir',
    include_ignored: true,
    max_results: 100
  }, meta);
  assert.equal(directHiddenBlocked.ok, true, JSON.stringify(directHiddenBlocked));
  assert.deepEqual(directHiddenBlocked.entries, []);

  const directHidden = await callTool(ctx, 'list_files', {
    path: '.hidden-dir',
    include_hidden: true,
    include_ignored: true,
    max_results: 100
  }, meta);
  assert.equal(directHidden.ok, true, JSON.stringify(directHidden));
  assert.deepEqual(directHidden.entries.map(entry => entry.path), ['.hidden-dir/inside.txt']);

  const directDoubleDot = await callTool(ctx, 'list_files', {
    path: '..double-dot.txt',
    include_hidden: true,
    include_ignored: true,
    max_results: 100
  }, meta);
  assert.equal(directDoubleDot.ok, false);
  assert.equal(directDoubleDot.error.code, 'NOT_A_DIRECTORY');
  assert.equal(directDoubleDot.error.category, 'validation');
  assert.equal(directDoubleDot.error.message, 'Path is not a directory.');

  const outside = await callTool(ctx, 'list_files', {
    path: '../outside',
    include_hidden: true,
    include_ignored: true,
    max_results: 100
  }, meta);
  assert.equal(outside.ok, false);
  assert.equal(outside.error.code, 'PATH_OUTSIDE_WORKSPACE');
});

test('linked worktree .git pointer honors shared info exclude', async t => {
  const { root, ctx, meta } = await fixture(t);
  const commonGit = await mkdtemp(path.join(tmpdir(), 'ctmcp-ignore-common-git-'));
  t.after(() => rm(commonGit, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const admin = path.join(commonGit, 'worktrees', 'linked');
  await mkdir(path.join(commonGit, 'info'), { recursive: true });
  await mkdir(admin, { recursive: true });
  await writeFile(path.join(root, '.git'), `gitdir: ${admin}\n`);
  await writeFile(path.join(admin, 'gitdir'), `${path.join(root, '.git')}\n`);
  await writeFile(path.join(admin, 'commondir'), '../..\n');
  await writeFile(path.join(commonGit, 'info', 'exclude'), 'worktree-ignored.txt\n');
  await put(root, 'visible.txt', 'needle visible\n');
  await put(root, 'worktree-ignored.txt', 'needle ignored\n');

  assert.deepEqual(await listedPaths(ctx, meta), ['visible.txt']);

  const search = await callTool(ctx, 'search_text', { query: 'needle', max_results: 100 }, meta);
  assert.equal(search.ok, true, JSON.stringify(search));
  assert.deepEqual(search.matches.map(match => match.path), ['visible.txt']);

  assert.deepEqual(
    await listedPaths(ctx, meta, { include_ignored: true }),
    ['visible.txt', 'worktree-ignored.txt']
  );
});

test('linked worktree metadata pointer requires a reciprocal gitdir backlink', async t => {
  const { root, ctx, meta } = await fixture(t);
  const commonGit = await mkdtemp(path.join(tmpdir(), 'ctmcp-ignore-untrusted-git-'));
  t.after(() => rm(commonGit, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  const admin = path.join(commonGit, 'worktrees', 'linked');
  await mkdir(path.join(commonGit, 'info'), { recursive: true });
  await mkdir(admin, { recursive: true });
  await writeFile(path.join(root, '.git'), `gitdir: ${admin}\n`);
  await writeFile(path.join(admin, 'gitdir'), `${path.join(root, 'not-the-dotgit-file')}\n`);
  await writeFile(path.join(admin, 'commondir'), '../..\n');
  await writeFile(path.join(commonGit, 'info', 'exclude'), 'must-remain-visible.txt\n');
  await put(root, 'must-remain-visible.txt', 'needle visible\n');

  assert.deepEqual(await listedPaths(ctx, meta), ['must-remain-visible.txt']);
});

test('project map, search and formatter project scope share ignore-aware traversal', async t => {
  const { root, ctx, meta } = await fixture(t);
  await put(root, '.gitignore', 'ignored/\nignored.json\n');
  await put(root, 'package.json', JSON.stringify({ scripts: { test: 'node --test' } }));
  await put(root, 'visible.txt', 'needle visible\n');
  await put(root, 'visible.json', '{"b":2,"a":1}\n');
  await put(root, 'ignored/hidden.txt', 'needle hidden\n');
  await put(root, 'ignored/package.json', JSON.stringify({ scripts: { ignoredOnly: 'echo ignored' } }));
  await put(root, 'ignored.json', '{"ignored":true}\n');

  const project = await callTool(ctx, 'project_map', { max_depth: 4 }, meta);
  assert.equal(project.ok, true, JSON.stringify(project));
  assert.deepEqual(project.manifests.map(item => item.path), ['package.json']);
  assert.equal(project.package_scripts.ignoredOnly, undefined);
  assert.ok(!project.tree.some(item => item.path.startsWith('ignored')));

  const projectAll = await callTool(ctx, 'project_map', { max_depth: 4, include_ignored: true }, meta);
  assert.equal(projectAll.ok, true, JSON.stringify(projectAll));
  assert.deepEqual(projectAll.manifests.map(item => item.path), ['ignored/package.json', 'package.json']);
  assert.equal(projectAll.package_scripts.ignoredOnly, 'echo ignored');

  const search = await callTool(ctx, 'search_text', { query: 'needle' }, meta);
  assert.deepEqual(search.matches.map(match => match.path), ['visible.txt']);
  const searchAll = await callTool(ctx, 'search_text', { query: 'needle', include_ignored: true }, meta);
  assert.deepEqual(searchAll.matches.map(match => match.path), ['ignored/hidden.txt', 'visible.txt']);

  const plan = await callTool(ctx, 'format_files', { scope: 'project', mode: 'plan' }, meta);
  assert.equal(plan.ok, true, JSON.stringify(plan));
  assert.ok(plan.selection.some(item => item.path === 'visible.json'));
  assert.ok(plan.selection.some(item => item.path === 'package.json'));
  assert.ok(!plan.selection.some(item => item.path === 'ignored.json'));
  assert.ok(!plan.selection.some(item => item.path === 'ignored/package.json'));
});

test('walker reports symlinks without traversing their targets', async t => {
  const { root, ctx, meta } = await fixture(t);
  const external = await mkdtemp(path.join(tmpdir(), 'ctmcp-ignore-outside-'));
  t.after(() => rm(external, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }));
  await put(external, 'outside.txt');
  try {
    await symlink(external, path.join(root, 'external-link'), process.platform === 'win32' ? 'junction' : 'dir');
  } catch (error) {
    if (['EPERM', 'EACCES', 'ENOTSUP'].includes(error?.code)) {
      t.skip(`symlinks unavailable: ${error.code}`);
      return;
    }
    throw error;
  }

  const result = await callTool(ctx, 'list_files', {
    path: '.',
    include_hidden: true,
    include_ignored: true,
    entry_types: ['file', 'symlink']
  }, meta);
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.ok(result.entries.some(entry => entry.path === 'external-link' && entry.type === 'symlink'));
  assert.ok(!result.entries.some(entry => entry.path === 'external-link/outside.txt'));
});
