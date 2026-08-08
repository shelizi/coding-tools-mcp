import type { JsonObject, ToolContext } from './types.js';
import { runBuffered } from './processes.js';
import { createHash } from 'node:crypto';
import { rm, stat } from 'node:fs/promises';
import { resolveInside, resolveExistingPath, rejectProtectedWritePath, rootAndCwd, validateWorkspaceUserPath } from './workspace.js';

const ok = (value: JsonObject): JsonObject => ({ ok: true, ...value });
const fail = (code: string, message: string): JsonObject => ({ ok: false, error: { code, message, category: 'git', retryable: false } });
const MAX_GIT_RESTORE_SNAPSHOT_BYTES = 16 * 1024 * 1024;

type GitResult = { code: number | null; stdout: string; stderr: string };
type GitTarget = {
  repoPath: string;
  root: string;
  gitDir: string;
  commonDir: string;
  branch: string;
  head: string;
  fingerprint: string;
};

async function runGitAt(cwd: string, args: string[], timeoutMs = 30_000, input?: string): Promise<GitResult> {
  return runBuffered('git', args, cwd, input, timeoutMs, { ...process.env, GIT_TERMINAL_PROMPT: '0' });
}

async function runGit(ctx: ToolContext, key: string, args: string[], timeoutMs = 30_000, input?: string): Promise<GitResult> {
  return runGitAt(rootAndCwd(ctx, key).root, args, timeoutMs, input);
}

function targetMetadata(target: GitTarget): JsonObject {
  return {
    repo_path: target.repoPath,
    repo_root: target.root,
    git_dir: target.gitDir,
    git_common_dir: target.commonDir,
    branch: target.branch,
    head: target.head,
    repo_fingerprint: target.fingerprint
  };
}

async function resolveGitTarget(ctx: ToolContext, key: string, args: JsonObject): Promise<GitTarget> {
  const workspaceRoot = rootAndCwd(ctx, key).root;
  const repoPath = String(args.repo_path ?? '.');
  const resolved = await resolveExistingPath(workspaceRoot, repoPath);
  const info = await stat(resolved.full);
  if (!info.isDirectory()) throw new Error('repo_path must be a directory');
  const values = await Promise.all([
    runGitAt(resolved.full, ['rev-parse', '--show-toplevel'], 5_000),
    runGitAt(resolved.full, ['rev-parse', '--absolute-git-dir'], 5_000),
    runGitAt(resolved.full, ['rev-parse', '--git-common-dir'], 5_000),
    runGitAt(resolved.full, ['rev-parse', '--abbrev-ref', 'HEAD'], 5_000),
    runGitAt(resolved.full, ['rev-parse', 'HEAD'], 5_000)
  ]);
  if (values[0].code !== 0) throw new Error('NOT_GIT_REPOSITORY');
  const [root, gitDir, commonDir, branch, head] = values.map(result => result.stdout.trim());
  const fingerprint = createHash('sha256')
    .update([root, gitDir, commonDir || gitDir, branch || 'HEAD', head || 'missing'].join('\0'))
    .digest('hex');
  return { repoPath, root, gitDir, commonDir: commonDir || gitDir, branch, head, fingerprint };
}

function repoTargetMismatch(target: GitTarget, expected: unknown): JsonObject | null {
  if (!expected || String(expected).toLowerCase() === target.fingerprint.toLowerCase()) return null;
  return {
    ok: false,
    error: {
      code: 'GIT_REPO_TARGET_MISMATCH',
      message: 'Git repository/worktree target changed since preflight',
      category: 'conflict',
      retryable: true,
      details: {
        expected_repo_fingerprint: String(expected),
        actual_repo_fingerprint: target.fingerprint,
        repo: targetMetadata(target),
        suggestion: 'Call git_status with path=repo_path and retry with the returned repo_fingerprint'
      }
    }
  };
}

function boundedInteger(value: unknown, fallback: number, minimum: number, maximum: number): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? Math.max(minimum, Math.min(maximum, Math.trunc(parsed))) : fallback;
}

async function resolveExistingGitPath(ctx: ToolContext, key: string, value: unknown): Promise<{
  root: string;
  full: string;
  display: string;
  isDirectory: boolean;
}> {
  const resolved = await resolveExistingPath(rootAndCwd(ctx, key).root, String(value ?? '.'));
  const info = await stat(resolved.full);
  return {
    root: resolved.root,
    full: resolved.full,
    display: resolved.display,
    isDirectory: info.isDirectory()
  };
}

function gitPaths(ctx: ToolContext, key: string, args: JsonObject, protectedWrites = false, target?: GitTarget): string[] {
  rootAndCwd(ctx, key);
  const values = Array.isArray(args.paths) ? args.paths.map(String) : args.path ? [String(args.path)] : [];
  return values.map(value => {
    validateWorkspaceUserPath(value, { allowDot: false });
    if (protectedWrites) {
      const workspacePath = target && target.repoPath !== '.' ? `${target.repoPath.replace(/[\\/]+$/, '')}/${value}` : value;
      rejectProtectedWritePath(workspacePath);
    }
    return value.replaceAll('\\', '/');
  });
}

function validateGitRef(value: unknown, fallback = 'HEAD'): string {
  const ref = String(value ?? fallback);
  if (!ref || ref.startsWith('-') || /[\0\r\n]/.test(ref)) throw new Error('INVALID_ARGUMENT');
  return ref;
}

async function isGitRepoAt(cwd: string): Promise<boolean> {
  const result = await runGitAt(cwd, ['rev-parse', '--git-dir'], 5_000);
  return result.code === 0;
}

function parseBranchLine(line: string): { branch: string; upstream: string; ahead: number; behind: number } {
  const separator = line.indexOf('...');
  const branchPart = separator >= 0 ? line.slice(0, separator) : line;
  const tracking = separator >= 0 ? line.slice(separator + 3) : '';
  const branch = branchPart.includes(' ') ? branchPart.slice(0, branchPart.indexOf(' ')) : branchPart;
  let upstream = tracking;
  let ahead = 0;
  let behind = 0;
  const metadataAt = tracking.indexOf(' ');
  if (metadataAt >= 0) {
    upstream = tracking.slice(0, metadataAt);
    const metadata = tracking.slice(metadataAt + 1).replace(/^\[/, '').replace(/\]$/, '');
    for (const item of metadata.split(',')) {
      const token = item.trim();
      if (token.startsWith('ahead ')) ahead = Number.parseInt(token.slice(6).trim(), 10) || 0;
      else if (token.startsWith('behind ')) behind = Number.parseInt(token.slice(7).trim(), 10) || 0;
    }
  }
  return { branch, upstream, ahead, behind };
}

function parseDiffFiles(diff: string): JsonObject[] {
  const files: JsonObject[] = [];
  for (const line of diff.split(/\r?\n/)) {
    if (line.startsWith('+++ b/')) {
      files.push({ path: line.slice(6), status: 'modified', binary: false });
    } else if (line.startsWith('--- /dev/null')) {
      continue;
    } else if (line.startsWith('--- a/')) {
      const filePath = line.slice(6);
      if (!files.some(file => file.path === filePath)) files.push({ path: filePath, status: 'modified', binary: false });
    }
  }
  return files;
}

function parseGitBlamePorcelain(output: string): JsonObject[] {
  const rows: JsonObject[] = [];
  let current: JsonObject = {};
  for (const raw of output.split(/\r?\n/)) {
    const parts = raw.trim().split(/\s+/);
    if (parts.length >= 3 && /^[0-9a-fA-F^]{40}/.test(parts[0])) {
      current = { commit: parts[0].replace(/^\^/, '') };
      if (/^\d+$/.test(parts[1])) current.original_line = Number(parts[1]);
      if (/^\d+$/.test(parts[2])) current.line = Number(parts[2]);
      continue;
    }
    if (raw.startsWith('author ')) current.author = raw.slice(7);
    else if (raw.startsWith('author-mail ')) current.author_mail = raw.slice(12).replace(/^</, '').replace(/>$/, '');
    else if (raw.startsWith('author-time ')) {
      const value = raw.slice(12);
      current.author_time = /^\d+$/.test(value) ? Number(value) : value;
    } else if (raw.startsWith('summary ')) current.summary = raw.slice(8);
    else if (raw.startsWith('\t')) rows.push({ ...current, content: raw.slice(1) });
  }
  return rows;
}

function gitReadFailure(result: GitResult): JsonObject {
  return fail('GIT_ERROR', (result.stderr || result.stdout || 'Git command failed').trim());
}

async function runGitDiff(root: string, context: number, paths: string[], cached: boolean): Promise<GitResult> {
  const argv = ['diff', `--unified=${context}`];
  if (cached) argv.push('--cached');
  if (paths.length) argv.push('--', ...paths);
  return runGitAt(root, argv, 60_000);
}

async function gitIndexClean(ctx: ToolContext, key: string): Promise<boolean> {
  const result = await runGit(ctx, key, ['diff', '--cached', '--quiet', '--exit-code']);
  if (result.code === 0) return true;
  if (result.code === 1) return false;
  throw new Error(result.stderr || 'GIT_INDEX_CHECK_FAILED');
}

async function validateBranchName(ctx: ToolContext, key: string, name: string): Promise<void> {
  const result = await runGit(ctx, key, ['check-ref-format', '--branch', name]);
  if (result.code !== 0) throw new Error('INVALID_GIT_BRANCH_NAME');
}

async function ensureExpectedHead(target: GitTarget, expected: unknown): Promise<void> {
  if (!expected) return;
  if (target.head !== String(expected).trim()) throw new Error('EXPECTED_HEAD_MISMATCH');
}

export async function gitStatusTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const resolved = await resolveExistingGitPath(ctx, key, args.path ?? '.');
  const rootCheck = await runGitAt(resolved.full, ['rev-parse', '--show-toplevel'], 10_000);
  if (rootCheck.code !== 0) {
    return ok({
      is_repo: false,
      clean: true,
      entries: [],
      warnings: [(rootCheck.stderr || rootCheck.stdout || 'not a git repository').trim()]
    });
  }
  const argv = ['status', '--porcelain=v1', '-b'];
  if (args.include_untracked === false) argv.push('--untracked-files=no');
  const result = await runGitAt(resolved.full, argv, 10_000);
  if (result.code !== 0) return gitReadFailure(result);
  const lines = result.stdout.split(/\r?\n/).filter(Boolean);
  const totalLines = lines.length;
  const maxEntries = boundedInteger(args.max_entries, 1_000, 1, 10_000);
  let branch = '';
  let upstream = '';
  let ahead = 0;
  let behind = 0;
  const entries: JsonObject[] = [];
  for (const line of lines) {
    if (line.startsWith('## ')) {
      ({ branch, upstream, ahead, behind } = parseBranchLine(line.slice(3)));
      continue;
    }
    if (line.length < 4) continue;
    const pathText = line.slice(3);
    const renameAt = pathText.indexOf(' -> ');
    const entry: JsonObject = {
      path: renameAt >= 0 ? pathText.slice(renameAt + 4) : pathText,
      index_status: line[0] ?? ' ',
      worktree_status: line[1] ?? ' '
    };
    if (renameAt >= 0) entry.original_path = pathText.slice(0, renameAt);
    entries.push(entry);
    if (entries.length >= maxEntries) break;
  }
  const headResult = await runGitAt(resolved.full, ['rev-parse', 'HEAD'], 5_000);
  const target = await resolveGitTarget(ctx, key, { repo_path: args.path ?? '.' });
  return ok({
    is_repo: true,
    branch,
    head: headResult.code === 0 ? headResult.stdout.trim() : '',
    upstream,
    ahead,
    behind,
    clean: entries.length === 0,
    repo: targetMetadata(target),
    repo_fingerprint: target.fingerprint,
    entries,
    truncated: entries.length >= maxEntries && totalLines > maxEntries + 1,
    warnings: []
  });
}

export async function gitDiffTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const { root } = rootAndCwd(ctx, key);
  const requestedContext = boundedInteger(args.context_lines, 3, 0, 1_000);
  const context = Math.min(requestedContext, 20);
  const maxBytes = boundedInteger(args.max_bytes, 262_144, 1_024, 1_048_576);
  const staged = args.staged === true;
  const unstaged = args.unstaged !== false;
  const paths = gitPaths(ctx, key, args);
  if (!(await isGitRepoAt(root))) {
    return ok({ diff: '', files: [], truncated: false, warnings: ['not a git repository'] });
  }
  const chunks: string[] = [];
  if (unstaged) {
    const result = await runGitDiff(root, context, paths, false);
    if (result.code !== 0 && result.code !== 1) return gitReadFailure(result);
    chunks.push(result.stdout);
  }
  if (staged) {
    const result = await runGitDiff(root, context, paths, true);
    if (result.code !== 0 && result.code !== 1) return gitReadFailure(result);
    chunks.push(result.stdout);
  }
  let combined = chunks.join('\n');
  if (combined && !combined.endsWith('\n')) combined += '\n';
  const data = Buffer.from(combined);
  const truncated = data.length > maxBytes;
  const diff = truncated ? data.subarray(0, maxBytes).toString('utf8') : combined;
  const normalized = requestedContext !== context;
  return ok({
    diff,
    files: parseDiffFiles(diff),
    arguments_normalized: normalized,
    normalized_arguments: normalized ? { context_lines: context } : null,
    truncated,
    warnings: truncated ? ['diff truncated'] : [],
    bytes: data.length
  });
}

export async function gitLogTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const resolved = await resolveExistingGitPath(ctx, key, args.path ?? '.');
  const ref = validateGitRef(args.ref, 'HEAD');
  const maxCount = boundedInteger(args.max_count, 20, 1, 100);
  const skip = boundedInteger(args.skip, 0, 0, 10_000);
  if (!(await isGitRepoAt(resolved.root))) {
    return ok({ is_repo: false, commits: [], truncated: false, warnings: [] });
  }
  const argv = [
    'log', `--max-count=${maxCount + 1}`, `--skip=${skip}`, '--date=iso-strict',
    '--pretty=format:%H%x1f%h%x1f%an%x1f%ae%x1f%ad%x1f%s%x1e', ref
  ];
  if (resolved.display !== '.') argv.push('--', resolved.display);
  const result = await runGitAt(resolved.root, argv, 10_000);
  if (result.code !== 0) return gitReadFailure(result);
  const commits = result.stdout.split('\x1e').flatMap(record => {
    const fields = record.trim().split('\x1f').map(value => value.trim());
    if (fields.length < 6 || !fields[0]) return [];
    const [hash, short_hash, author_name, author_email, author_date, subject] = fields;
    return [{ hash, short_hash, author_name, author_email, author_date, authored_at: author_date, subject }];
  });
  const truncated = commits.length > maxCount;
  const page = commits.slice(0, maxCount);
  return ok({
    is_repo: true,
    ref,
    path: resolved.display,
    commits: page,
    truncated,
    warnings: truncated ? ['commit limit reached'] : [],
    count: page.length,
    skip,
    max_count: maxCount
  });
}

export async function gitShowTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const { root } = rootAndCwd(ctx, key);
  if (!(await isGitRepoAt(root))) {
    return ok({ is_repo: false, content: '', files: [], truncated: false, warnings: [] });
  }
  const rev = validateGitRef(args.rev, 'HEAD');
  const requestedContext = boundedInteger(args.context_lines, 3, 0, 1_000);
  const context = Math.min(requestedContext, 20);
  const maxBytes = boundedInteger(args.max_bytes, 262_144, 1, 1_048_576);
  const includeDiff = args.include_diff !== false;
  const paths = gitPaths(ctx, key, args);
  const argv = ['show', '--no-ext-diff', '--format=fuller', `--unified=${context}`];
  if (!includeDiff) argv.push('--no-patch');
  argv.push(rev);
  if (paths.length) argv.push('--', ...paths);
  const result = await runGitAt(root, argv, 60_000);
  if (result.code !== 0) return gitReadFailure(result);
  const data = Buffer.from(result.stdout);
  const truncated = data.length > maxBytes;
  const content = truncated ? data.subarray(0, maxBytes).toString('utf8') : result.stdout;
  const normalized = requestedContext !== context;
  return ok({
    is_repo: true,
    rev,
    content,
    output: content,
    files: parseDiffFiles(content),
    arguments_normalized: normalized,
    normalized_arguments: normalized ? { context_lines: context } : null,
    truncated,
    output_bytes: Buffer.byteLength(content),
    bytes: data.length,
    warnings: truncated ? ['output truncated'] : []
  });
}

export async function gitBlameTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const requestedPath = String(args.path ?? '').trim();
  if (!requestedPath) throw new Error('path is required');
  const resolved = await resolveExistingGitPath(ctx, key, requestedPath);
  if (resolved.isDirectory) throw new Error('IS_DIRECTORY');
  if (!(await isGitRepoAt(resolved.root))) {
    return ok({ is_repo: false, path: resolved.display, lines: [], truncated: false, warnings: [] });
  }
  const rev = args.rev === undefined ? null : validateGitRef(args.rev);
  const maxLines = boundedInteger(args.max_lines, 200, 1, 1_000);
  const maximumStartLine = Number.MAX_SAFE_INTEGER - maxLines + 1;
  const startLine = boundedInteger(args.start_line, 1, 1, maximumStartLine);
  let finalLine = args.end_line === undefined
    ? startLine + maxLines - 1
    : boundedInteger(args.end_line, startLine, 1, Number.MAX_SAFE_INTEGER);
  if (finalLine < startLine) throw new Error('INVALID_ARGUMENT');
  const requestedLines = finalLine - startLine + 1;
  let truncated = requestedLines > maxLines;
  finalLine = Math.min(finalLine, startLine + maxLines - 1);
  const argv = ['blame', '--line-porcelain', '-L', `${startLine},${finalLine}`];
  if (rev) argv.push(rev);
  argv.push('--', resolved.display);
  const result = await runGitAt(resolved.root, argv, 60_000);
  if (result.code !== 0) return gitReadFailure(result);
  let lines = parseGitBlamePorcelain(result.stdout);
  if (lines.length > maxLines) {
    lines = lines.slice(0, maxLines);
    truncated = true;
  }
  return ok({
    is_repo: true,
    path: resolved.display,
    rev,
    start_line: startLine,
    end_line: finalLine,
    lines,
    truncated,
    warnings: truncated ? ['line limit reached'] : []
  });
}

export async function gitBranchTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const target = await resolveGitTarget(ctx, key, args);
  const mismatch = repoTargetMismatch(target, args.expected_repo_fingerprint);
  if (mismatch) return mismatch;
  await ensureExpectedHead(target, args.expected_head);
  const action = String(args.action ?? '');
  const name = String(args.name ?? '');
  if (!name) throw new Error('name is required');
  await validateBranchName(ctx, key, name);
  let argv: string[];
  if (action === 'create') {
    const start = String(args.start_point ?? 'HEAD');
    argv = args.switch === false ? ['branch', name, start] : ['switch', '-c', name, start];
  }
  else if (action === 'switch') argv = ['switch', name];
  else if (action === 'delete') {
    if (ctx.config.securityPolicy.requireWriteConfirmation && args.confirm !== true) return fail('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION', 'Deleting a branch requires confirm=true');
    argv = ['branch', args.force === true ? '-D' : '-d', name];
  } else throw new Error('invalid git branch action');
  const command = ['git', ...argv];
  if (args.dry_run === true) return ok({ dry_run: true, applied: false, action, name, command, repo: targetMetadata(target), warnings: [] });
  const result = await runGitAt(target.root, argv, 60_000);
  if (result.code !== 0) return fail('GIT_BRANCH_FAILED', result.stderr || result.stdout);
  return ok({
    dry_run: false,
    applied: true,
    action,
    name,
    command,
    stdout: result.stdout.trim(),
    repo: targetMetadata(target),
    status: await gitStatusTool(ctx, key, { path: target.repoPath }),
    affected_files: [],
    warnings: []
  });
}

export async function gitStageTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const preflightPaths = gitPaths(ctx, key, args, true);
  const target = await resolveGitTarget(ctx, key, args);
  const mismatch = repoTargetMismatch(target, args.expected_repo_fingerprint);
  if (mismatch) return mismatch;
  await ensureExpectedHead(target, args.expected_head);
  const paths = target.repoPath === '.' ? preflightPaths : gitPaths(ctx, key, args, true, target);
  if (args.all !== true && !paths.length) throw new Error('paths or all=true is required');
  const argv = args.all === true ? ['add', '-A'] : ['add', '--', ...paths];
  if (args.dry_run === true) return ok({ dry_run: true, applied: false, paths, all: args.all === true, command: ['git', ...argv], repo: targetMetadata(target), warnings: [] });
  const result = await runGitAt(target.root, argv);
  if (result.code !== 0) return fail('GIT_STAGE_FAILED', result.stderr || result.stdout);
  return ok({
    dry_run: false,
    applied: true,
    paths,
    all: args.all === true,
    repo: targetMetadata(target),
    status: await gitStatusTool(ctx, key, { path: target.repoPath }),
    affected_files: paths.map(file => ({ path: file, operation: 'stage' })),
    warnings: []
  });
}

export async function gitCommitTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const preflightPaths = gitPaths(ctx, key, args, true);
  const target = await resolveGitTarget(ctx, key, args);
  const mismatch = repoTargetMismatch(target, args.expected_repo_fingerprint);
  if (mismatch) return mismatch;
  await ensureExpectedHead(target, args.expected_head);
  const message = String(args.message ?? '');
  if (!message.trim() || message.length > 10_000) throw new Error('message must be between 1 and 10000 characters');
  const paths = target.repoPath === '.' ? preflightPaths : gitPaths(ctx, key, args, true, target);
  const all = args.all === true;
  const stagedByTool = paths.length > 0 || all;
  const requireCleanIndex = args.require_clean_index_before === undefined ? stagedByTool : args.require_clean_index_before === true;
  const indexClean = (await runGitAt(target.root, ['diff', '--cached', '--quiet', '--exit-code'], 10_000)).code === 0;
  if (requireCleanIndex && !indexClean) {
    return {
      ok: false,
      error: {
        code: 'GIT_INDEX_NOT_CLEAN',
        message: 'git_commit requires a clean index before staging paths',
        category: 'conflict',
        retryable: false,
        details: { suggestion: 'commit or unstage existing staged changes, or set require_clean_index_before=false' }
      }
    };
  }
  if (args.dry_run === true) return ok({ dry_run: true, applied: false, message, paths, all, index_clean: indexClean, repo: targetMetadata(target), warnings: [] });

  const oldHead = await runGitAt(target.root, ['rev-parse', 'HEAD']);
  if (stagedByTool) {
    const staged = await runGitAt(target.root, all ? ['add', '-A'] : ['add', '--', ...paths]);
    if (staged.code !== 0) return fail('GIT_STAGE_FAILED', staged.stderr || staged.stdout);
  }
  const argv = ['commit', '-m', message];
  if (args.allow_empty === true) argv.push('--allow-empty');
  const result = await runGitAt(target.root, argv, 60_000);
  if (result.code !== 0) {
    let indexRestored = false;
    if (stagedByTool && indexClean) {
      const reset = await runGitAt(target.root, ['reset', '--quiet', 'HEAD', '--']);
      indexRestored = reset.code === 0;
    }
    return {
      ok: false,
      error: {
        code: 'GIT_COMMIT_FAILED',
        message: (result.stderr || result.stdout).trim(),
        category: 'runtime',
        retryable: false,
        details: { stdout: result.stdout, staged_by_tool: stagedByTool, index_restored: indexRestored }
      }
    };
  }
  const head = await runGitAt(target.root, ['rev-parse', 'HEAD']);
  return ok({
    dry_run: false,
    applied: true,
    commit: head.stdout.trim(),
    previous_head: oldHead.code === 0 ? oldHead.stdout.trim() : '',
    message,
    paths,
    all,
    stdout: result.stdout.trim(),
    repo: targetMetadata(target),
    status: await gitStatusTool(ctx, key, { path: target.repoPath }),
    affected_files: paths.map(file => ({ path: file, operation: 'commit' })),
    warnings: []
  });
}

export async function gitPushTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const target = await resolveGitTarget(ctx, key, args);
  const mismatch = repoTargetMismatch(target, args.expected_repo_fingerprint);
  if (mismatch) return mismatch;
  await ensureExpectedHead(target, args.expected_head);
  const remote = validateGitRef(args.remote, 'origin');
  const branch = args.branch !== undefined
    ? validateGitRef(args.branch)
    : target.branch !== 'HEAD'
      ? target.branch
      : '';
  if (!branch) throw new Error('branch is required when HEAD is detached');
  const dryRun = args.dry_run === true;
  const argv = ['push'];
  if (dryRun) argv.push('--dry-run');
  if (args.set_upstream === true) argv.push('--set-upstream');
  argv.push(remote, branch);
  const result = await runGitAt(target.root, argv, 120_000);
  if (result.code !== 0) {
    const message = (result.stderr || result.stdout).trim();
    const lowered = message.toLowerCase();
    if (lowered.includes('authentication failed') || lowered.includes('failed to authenticate') || lowered.includes('could not read username')) {
      return {
        ok: false,
        error: {
          code: 'GIT_AUTHENTICATION_FAILED',
          message,
          category: 'authentication',
          retryable: true,
          details: {
            remote,
            branch,
            suggestion: 'Refresh the Git credential/token for this remote, then retry the same git_push request.'
          }
        }
      };
    }
    return {
      ok: false,
      error: {
        code: 'GIT_PUSH_FAILED',
        message,
        category: 'git',
        retryable: false,
        details: { remote, branch, stdout: result.stdout.trim() }
      }
    };
  }
  return ok({
    dry_run: dryRun,
    applied: !dryRun,
    remote,
    branch,
    stdout: result.stdout.trim(),
    stderr: result.stderr.trim(),
    repo: targetMetadata(target),
    status: await gitStatusTool(ctx, key, { path: target.repoPath }),
    affected_files: [],
    warnings: []
  });
}

export async function gitRuntimeRevisionInfo(ctx: ToolContext, key: string): Promise<JsonObject> {
  try {
    const target = await resolveGitTarget(ctx, key, { repo_path: '.' });
    const committed = await runGitAt(target.root, ['show', '-s', '--format=%ct', 'HEAD'], 5_000);
    const committedAtMs = committed.code === 0 ? Number.parseInt(committed.stdout.trim(), 10) * 1000 : NaN;
    return {
      workspace_git_head: target.head,
      workspace_head_committed_at_ms: Number.isFinite(committedAtMs) ? committedAtMs : null
    };
  } catch {
    return { workspace_git_head: null, workspace_head_committed_at_ms: null };
  }
}

export interface GitRestoreSnapshot {
  head: string;
  paths: string[];
  changedPaths: string[];
  addedPaths: string[];
  stagedPatch: string;
  worktreePatch: string;
  stagedBytes: number;
  worktreeBytes: number;
  totalBytes: number;
}

function nulPaths(value: string): string[] {
  return value.split('\0').filter(Boolean).map(item => item.replaceAll('\\', '/'));
}

export async function captureGitRestoreSnapshot(ctx: ToolContext, key: string, paths: string[], repoRoot = rootAndCwd(ctx, key).root): Promise<GitRestoreSnapshot> {
  const head = await runGitAt(repoRoot, ['rev-parse', 'HEAD']);
  if (head.code !== 0) throw new Error('GIT_RESTORE_SNAPSHOT_FAILED');
  const common = ['--binary', '--full-index', '--no-ext-diff', '--', ...paths];
  const staged = await runGitAt(repoRoot, ['diff', '--cached', ...common], 60_000);
  const worktree = await runGitAt(repoRoot, ['diff', ...common], 60_000);
  const stagedNames = await runGitAt(repoRoot, ['diff', '--cached', '--name-only', '-z', '--', ...paths], 60_000);
  const worktreeNames = await runGitAt(repoRoot, ['diff', '--name-only', '-z', '--', ...paths], 60_000);
  const headNames = await runGitAt(repoRoot, ['ls-tree', '-r', '--name-only', '-z', head.stdout.trim(), '--', ...paths], 60_000);
  if ([staged, worktree, stagedNames, worktreeNames, headNames].some(result => result.code !== 0)) {
    throw new Error('GIT_RESTORE_SNAPSHOT_FAILED');
  }
  const stagedBytes = Buffer.byteLength(staged.stdout);
  const worktreeBytes = Buffer.byteLength(worktree.stdout);
  const totalBytes = stagedBytes + worktreeBytes;
  if (totalBytes > MAX_GIT_RESTORE_SNAPSHOT_BYTES) throw new Error('GIT_RESTORE_SNAPSHOT_TOO_LARGE');
  const changedPaths = [...new Set([...nulPaths(stagedNames.stdout), ...nulPaths(worktreeNames.stdout)])].sort();
  const headPathSet = new Set(nulPaths(headNames.stdout));
  const addedPaths = changedPaths.filter(item => !headPathSet.has(item));
  const currentHead = await runGitAt(repoRoot, ['rev-parse', 'HEAD']);
  if (currentHead.code !== 0 || currentHead.stdout.trim() !== head.stdout.trim()) throw new Error('GIT_HEAD_CHANGED_DURING_SNAPSHOT');
  return {
    head: head.stdout.trim(),
    paths: [...paths],
    changedPaths,
    addedPaths,
    stagedPatch: staged.stdout,
    worktreePatch: worktree.stdout,
    stagedBytes,
    worktreeBytes,
    totalBytes
  };
}

export async function restoreGitSnapshot(
  ctx: ToolContext,
  key: string,
  snapshot: GitRestoreSnapshot,
  repoRoot = rootAndCwd(ctx, key).root
): Promise<{ ok: boolean; steps: JsonObject[] }> {
  const steps: JsonObject[] = [];
  if (!snapshot.changedPaths.length) return { ok: true, steps };
  const added = new Set(snapshot.addedPaths);
  const trackedPaths = snapshot.changedPaths.filter(item => !added.has(item));
  const baselineErrors: string[] = [];
  if (trackedPaths.length) {
    const tracked = await runGitAt(repoRoot, [
      'restore', `--source=${snapshot.head}`, '--staged', '--worktree', '--', ...trackedPaths
    ], 60_000);
    if (tracked.code !== 0) baselineErrors.push(tracked.stderr.trim() || tracked.stdout.trim());
  }
  for (const addedPath of snapshot.addedPaths) {
    const index = await runGitAt(repoRoot, ['rm', '--cached', '--force', '--ignore-unmatch', '--', addedPath], 60_000);
    if (index.code !== 0) baselineErrors.push(index.stderr.trim() || index.stdout.trim());
    try { await rm(resolveInside(repoRoot, addedPath), { force: true, recursive: true }); }
    catch (error) { baselineErrors.push(error instanceof Error ? error.message : String(error)); }
  }
  steps.push({ step: 'baseline', ok: baselineErrors.length === 0, stderr: baselineErrors.filter(Boolean).join('\n') });
  if (baselineErrors.length) return { ok: false, steps };
  if (snapshot.stagedPatch) {
    const staged = await runGitAt(repoRoot, ['apply', '--index', '--binary', '--whitespace=nowarn', '-'], 60_000, snapshot.stagedPatch);
    steps.push({ step: 'staged_patch', ok: staged.code === 0, exit_code: staged.code, stderr: staged.stderr.trim() });
    if (staged.code !== 0) return { ok: false, steps };
  }
  if (snapshot.worktreePatch) {
    const worktree = await runGitAt(repoRoot, ['apply', '--binary', '--whitespace=nowarn', '-'], 60_000, snapshot.worktreePatch);
    steps.push({ step: 'worktree_patch', ok: worktree.code === 0, exit_code: worktree.code, stderr: worktree.stderr.trim() });
    if (worktree.code !== 0) return { ok: false, steps };
  }
  return { ok: true, steps };
}

export async function gitRestoreTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  const preflightPaths = gitPaths(ctx, key, args, true);
  const target = await resolveGitTarget(ctx, key, args);
  const mismatch = repoTargetMismatch(target, args.expected_repo_fingerprint);
  if (mismatch) return mismatch;
  await ensureExpectedHead(target, args.expected_head);
  if (ctx.config.securityPolicy.requireWriteConfirmation && args.confirm !== true) return fail('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION', 'git_restore discards or unstages changes and requires confirm=true');
  const paths = target.repoPath === '.' ? preflightPaths : gitPaths(ctx, key, args, true, target);
  if (!paths.length) throw new Error('paths are required');
  const argv = ['restore'];
  const staged = args.staged === true;
  const worktree = args.worktree === undefined ? !staged : args.worktree === true;
  if (!staged && !worktree) throw new Error('git_restore requires staged=true or worktree=true');
  if (staged) argv.push('--staged');
  if (worktree) argv.push('--worktree');
  if (args.source) argv.push(`--source=${String(args.source)}`);
  argv.push('--', ...paths);
  if (args.dry_run === true) return ok({ dry_run: true, applied: false, paths, staged, worktree, rollback_protected: true, repo: targetMetadata(target), warnings: [] });
  let snapshot: GitRestoreSnapshot;
  try {
    snapshot = await captureGitRestoreSnapshot(ctx, key, paths, target.root);
  } catch (error) {
    const code = error instanceof Error && /^[A-Z][A-Z0-9_]+$/.test(error.message)
      ? error.message
      : 'GIT_RESTORE_SNAPSHOT_FAILED';
    return fail(code, 'Unable to capture a bounded path-scoped Git restore snapshot');
  }
  await ensureExpectedHead(target, snapshot.head);
  const result = await runGitAt(target.root, argv, 60_000);
  if (result.code !== 0) {
    const rollback = await restoreGitSnapshot(ctx, key, snapshot, target.root);
    return {
      ok: false,
      error: {
        code: 'GIT_RESTORE_FAILED',
        message: (result.stderr || result.stdout).trim(),
        category: 'runtime',
        retryable: false,
        details: {
          rollback_protected: true,
          rollback_ok: rollback.ok,
          rollback_steps: rollback.steps,
          snapshot_bytes: snapshot.totalBytes,
          snapshot_staged_bytes: snapshot.stagedBytes,
          snapshot_worktree_bytes: snapshot.worktreeBytes
        }
      }
    };
  }
  return ok({
    dry_run: false,
    applied: true,
    paths,
    staged,
    worktree,
    rollback_protected: true,
    snapshot_bytes: snapshot.totalBytes,
    snapshot_staged_bytes: snapshot.stagedBytes,
    snapshot_worktree_bytes: snapshot.worktreeBytes,
    repo: targetMetadata(target),
    status: await gitStatusTool(ctx, key, { path: target.repoPath }),
    affected_files: paths.map(file => ({ path: file, operation: 'restore' })),
    warnings: []
  });
}
