import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, rm, rmdir, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import type { JsonObject, ToolContext } from './types.js';
import {
  exists, rejectDirectWriteSymlink, resolveWritePath, rootAndCwd, WorkspacePathError
} from './workspace.js';

interface PlannedDirectory {
  relative: string;
  full: string;
  existed: boolean;
  missing: string[];
}

class FileOpsError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly category = 'validation',
    readonly retryable = false,
    readonly details: JsonObject = {}
  ) {
    super(message);
  }
}

function ok(value: JsonObject): JsonObject { return { ok: true, ...value }; }
function fail(error: FileOpsError): JsonObject {
  return { ok: false, error: { code: error.code, message: error.message, category: error.category, retryable: error.retryable, details: error.details } };
}
function sha256(bytes: Buffer): string { return createHash('sha256').update(bytes).digest('hex'); }
function display(root: string, full: string): string { return path.relative(root, full).replaceAll('\\', '/') || '.'; }

function protectedPath(relative: string): boolean {
  const normalized = relative.replaceAll('\\', '/');
  const first = normalized.split('/')[0]?.toLowerCase();
  return first === '.git'
    || first === '.github'
    || first === '.coding-tools-format'
    || first === '.coding-tools-transaction';
}

function criticalFile(relative: string): boolean {
  const normalized = relative.replaceAll('\\', '/');
  const first = normalized.split('/')[0];
  if (first === '.git' || first === '.github') return true;
  const name = normalized.split('/').at(-1) ?? normalized;
  return name === '.gitignore'
    || name === 'Cargo.toml'
    || name === 'Cargo.lock'
    || name === 'package.json'
    || name === 'package-lock.json'
    || name === 'pnpm-lock.yaml'
    || name === 'tauri.conf.json'
    || name.startsWith('README')
    || name.startsWith('LICENSE')
    || name.startsWith('vite.config.')
    || name === 'pyproject.toml';
}

async function resolveWritable(root: string, relative: string): Promise<string> {
  if (!relative) throw new FileOpsError('INVALID_ARGUMENT', 'file operation path is required');
  try {
    const resolved = await resolveWritePath(root, relative);
    const normalized = relative.replaceAll('\\', '/');
    if (protectedPath(normalized)) throw new FileOpsError('PROTECTED_PATH', `Protected repository path cannot be modified: ${normalized}`, 'security', false);
    await rejectDirectWriteSymlink(resolved);
    return resolved.full;
  } catch (error) {
    if (error instanceof WorkspacePathError) {
      throw new FileOpsError(error.code, error.message, error.category, error.retryable, error.details);
    }
    throw error;
  }
}

async function optionalFile(full: string): Promise<Buffer | undefined> {
  try {
    const info = await stat(full);
    if (!info.isFile()) throw new FileOpsError('INVALID_ARGUMENT', `Target must be a file: ${full}`);
    return await readFile(full);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

function verifyExpected(operation: JsonObject, relative: string, bytes: Buffer | undefined): void {
  if (operation.expected_sha256 === undefined) return;
  const expected = String(operation.expected_sha256).toLowerCase();
  const actual = bytes ? sha256(bytes) : 'missing';
  if (expected !== actual) {
    throw new FileOpsError('FILE_VERSION_MISMATCH', `File changed since it was read: ${relative}`, 'conflict', true, {
      path: relative,
      expected_sha256: expected,
      actual_sha256: actual,
      suggestion: 'Read the file again and rebuild the file operation'
    });
  }
}

function simpleUnifiedDiff(relative: string, before: Buffer | undefined, after: Buffer | undefined): string {
  if (before?.equals(after ?? Buffer.alloc(0)) && after !== undefined) return '';
  const beforeText = before?.includes(0) ? undefined : before?.toString('utf8');
  const afterText = after?.includes(0) ? undefined : after?.toString('utf8');
  if (beforeText === undefined && before !== undefined) return '';
  if (afterText === undefined && after !== undefined) return '';
  const oldLines = (beforeText ?? '').split(/\r?\n/);
  const newLines = (afterText ?? '').split(/\r?\n/);
  return [
    `--- ${before === undefined ? '/dev/null' : `a/${relative}`}`,
    `+++ ${after === undefined ? '/dev/null' : `b/${relative}`}`,
    `@@ -1,${oldLines.length} +1,${newLines.length} @@`,
    ...oldLines.map(line => `-${line}`),
    ...newLines.map(line => `+${line}`),
    ''
  ].join('\n');
}

async function verifyVersions(root: string, versions: Map<string, string | null>): Promise<void> {
  for (const [relative, expected] of versions) {
    const full = await resolveWritable(root, relative);
    const current = await optionalFile(full);
    const actual = current ? sha256(current) : null;
    if (actual !== expected) {
      throw new FileOpsError('FILE_VERSION_MISMATCH', `File changed since preflight: ${relative}`, 'conflict', true, {
        path: relative,
        expected_sha256: expected ?? 'missing',
        actual_sha256: actual ?? 'missing',
        suggestion: 'Retry the complete file_ops transaction'
      });
    }
  }
}

async function restoreBackups(root: string, backups: Map<string, Buffer | null>): Promise<string[]> {
  const failures: string[] = [];
  for (const [relative, bytes] of backups) {
    const full = await resolveWritable(root, relative);
    try {
      if (bytes === null) await rm(full, { force: true });
      else {
        await mkdir(path.dirname(full), { recursive: true });
        await writeFile(full, bytes);
      }
    } catch { failures.push(relative); }
  }
  return failures;
}

async function missingDirectories(root: string, directory: string): Promise<string[]> {
  const missing: string[] = [];
  let current = directory;
  while (current !== root && current.startsWith(`${root}${path.sep}`)) {
    try {
      const info = await stat(current);
      if (!info.isDirectory()) throw new FileOpsError('INVALID_ARGUMENT', `Path component is not a directory: ${display(root, current)}`);
      break;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      missing.push(current);
      current = path.dirname(current);
    }
  }
  return missing.reverse();
}

async function removeCreatedDirectories(directories: Iterable<string>): Promise<void> {
  const ordered = [...new Set(directories)].sort((left, right) => right.split(path.sep).length - left.split(path.sep).length);
  for (const directory of ordered) await rmdir(directory).catch(() => undefined);
}

function versionOf(bytes: Buffer | undefined | null): string | null {
  return bytes ? sha256(bytes) : null;
}

async function commitStaged(
  root: string,
  staged: Map<string, Buffer | null>,
  versions: Map<string, string | null>
): Promise<{ backups: Map<string, Buffer | null>; createdDirectories: string[] }> {
  const backups = new Map<string, Buffer | null>();
  const temporary = new Map<string, string>();
  const createdDirectories = new Set<string>();
  let applying = false;
  try {
    for (const [relative, content] of staged) {
      const full = await resolveWritable(root, relative);
      const current = await optionalFile(full);
      const expected = versions.get(relative);
      const actual = versionOf(current);
      if (expected !== actual) {
        throw new FileOpsError('FILE_VERSION_MISMATCH', `File changed since preflight: ${relative}`, 'conflict', true, {
          path: relative,
          expected_sha256: expected ?? 'missing',
          actual_sha256: actual ?? 'missing',
          suggestion: 'Retry the complete file_ops transaction'
        });
      }
      backups.set(relative, current ?? null);
      if (content !== null) {
        const missing = await missingDirectories(root, path.dirname(full));
        await mkdir(path.dirname(full), { recursive: true });
        for (const directory of missing) createdDirectories.add(directory);
        const temp = path.join(path.dirname(full), `.${path.basename(full)}.node-agent-stage-${randomUUID()}`);
        await writeFile(temp, content, { flag: 'wx' });
        temporary.set(relative, temp);
      }
    }
    applying = true;
    for (const [relative, content] of staged) {
      const full = await resolveWritable(root, relative);
      const current = await optionalFile(full);
      const backup = backups.get(relative) ?? null;
      if (versionOf(current) !== versionOf(backup)) {
        throw new FileOpsError('FILE_VERSION_MISMATCH', `File changed while the transaction was being staged: ${relative}`, 'conflict', true, {
          path: relative,
          expected_sha256: versionOf(backup) ?? 'missing',
          actual_sha256: versionOf(current) ?? 'missing',
          suggestion: 'Retry the complete file_ops transaction'
        });
      }
      if (content === null) {
        await rm(full, { force: false });
        continue;
      }
      const temp = temporary.get(relative);
      if (!temp) throw new Error(`Staged file is missing: ${relative}`);
      if (process.platform === 'win32' && await exists(full)) await rm(full, { force: false });
      await rename(temp, full);
      temporary.delete(relative);
    }
    return { backups, createdDirectories: [...createdDirectories] };
  } catch (error) {
    await Promise.allSettled([...temporary.values()].map(file => rm(file, { force: true })));
    const rollbackFailures = await restoreBackups(root, backups);
    await removeCreatedDirectories(createdDirectories);
    if (error instanceof FileOpsError && error.code === 'FILE_VERSION_MISMATCH' && !applying) throw error;
    throw new FileOpsError('FILE_OPS_APPLY_FAILED', 'File transaction failed and was rolled back', 'runtime', true, {
      error: error instanceof Error ? error.message : String(error),
      rolled_back: [...backups.keys()].filter(file => !rollbackFailures.includes(file)),
      rollback_failures: rollbackFailures
    });
  }
}

export async function fileOpsTool(ctx: ToolContext, key: string, args: JsonObject): Promise<JsonObject> {
  try {
    const { root } = rootAndCwd(ctx, key);
    const operations = Array.isArray(args.operations) ? args.operations as JsonObject[] : [];
    if (!operations.length) throw new FileOpsError('INVALID_ARGUMENT', 'operations must not be empty');
    const dryRun = args.dry_run === true;
    const confirm = args.confirm === true;
    const staged = new Map<string, Buffer | null>();
    const versions = new Map<string, string | null>();
    const directories: PlannedDirectory[] = [];
    const affected: JsonObject[] = [];
    const touched = new Set<string>();
    let diff = '';

    for (let index = 0; index < operations.length; index += 1) {
      const operation = operations[index];
      const kind = String(operation.type ?? '');
      const requested = String(operation.path ?? '');
      const full = await resolveWritable(root, requested);
      const relative = display(root, full);
      if (!['create', 'delete', 'copy', 'move', 'mkdir'].includes(kind)) {
        throw new FileOpsError('INVALID_ARGUMENT', `unsupported file operation: ${kind}`, 'validation', false, { operation_index: index });
      }

      if (kind === 'mkdir') {
        const existed = await exists(full);
        if (existed && !(await stat(full)).isDirectory()) throw new FileOpsError('INVALID_ARGUMENT', `mkdir target is a file: ${relative}`);
        directories.push({ relative, full, existed, missing: existed ? [] : await missingDirectories(root, full) });
        affected.push({ path: relative, operation: 'mkdir' });
        continue;
      }

      if (kind === 'create') {
        const before = await optionalFile(full);
        const overwrite = operation.overwrite === true;
        if (before && overwrite && !confirm) throw new FileOpsError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION', `Overwriting an existing file requires confirm=true: ${relative}`, 'permission', false, { path: relative, operation_index: index });
        if (before && !overwrite) throw new FileOpsError('FILE_ALREADY_EXISTS', `Create target already exists: ${relative}`, 'conflict', false, { path: relative, operation_index: index });
        if (!touched.add(relative)) throw new FileOpsError('INVALID_ARGUMENT', `duplicate file_ops target: ${relative}`);
        verifyExpected(operation, relative, before);
        versions.set(relative, before ? sha256(before) : null);
        const content = Buffer.from(String(operation.content ?? ''), 'utf8');
        staged.set(relative, content);
        diff += simpleUnifiedDiff(relative, before, content);
        affected.push({ path: relative, operation: before ? 'update' : 'add' });
        continue;
      }

      const source = await optionalFile(full);
      if (!source) throw new FileOpsError('FILE_NOT_FOUND', `File not found: ${relative}`, 'not_found', false, { path: relative, operation_index: index });
      verifyExpected(operation, relative, source);

      if (kind === 'delete') {
        if (criticalFile(relative) && !confirm) throw new FileOpsError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION', `Deleting a critical project file requires confirm=true: ${relative}`, 'permission', false, { path: relative, operation_index: index });
        if (!touched.add(relative)) throw new FileOpsError('INVALID_ARGUMENT', `duplicate file_ops target: ${relative}`);
        versions.set(relative, sha256(source));
        staged.set(relative, null);
        diff += simpleUnifiedDiff(relative, source, undefined);
        affected.push({ path: relative, operation: 'delete' });
        continue;
      }

      const destinationValue = String(operation.destination ?? '');
      if (!destinationValue) throw new FileOpsError('INVALID_ARGUMENT', 'copy/move destination is required', 'validation', false, { operation_index: index });
      const destinationFull = await resolveWritable(root, destinationValue);
      const destination = display(root, destinationFull);
      if (destination === relative) throw new FileOpsError('INVALID_ARGUMENT', 'source and destination must differ', 'validation', false, { operation_index: index });
      const targetBefore = await optionalFile(destinationFull);
      const overwrite = operation.overwrite === true;
      if (targetBefore && overwrite && !confirm) throw new FileOpsError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION', `Overwriting a destination requires confirm=true: ${destination}`, 'permission', false, { path: destination, operation_index: index });
      if (targetBefore && !overwrite) throw new FileOpsError('FILE_ALREADY_EXISTS', `Destination already exists: ${destination}`, 'conflict', false, { path: destination, operation_index: index });
      if (!touched.add(destination) || (kind === 'move' && !touched.add(relative))) {
        throw new FileOpsError('INVALID_ARGUMENT', 'file_ops paths may only be touched once per transaction', 'validation', false, { operation_index: index });
      }
      versions.set(relative, sha256(source));
      versions.set(destination, targetBefore ? sha256(targetBefore) : null);
      staged.set(destination, source);
      if (kind === 'move') staged.set(relative, null);
      affected.push({ path: relative, destination, operation: kind });
    }

    if (!dryRun) {
      await verifyVersions(root, versions);
      const committed = await commitStaged(root, staged, versions);
      const createdDirectories = new Set(committed.createdDirectories);
      try {
        for (const directory of directories) {
          if (directory.existed) continue;
          await mkdir(directory.full, { recursive: true });
          for (const created of directory.missing) createdDirectories.add(created);
        }
      } catch (error) {
        const rollbackFailures = await restoreBackups(root, committed.backups);
        for (const directory of directories) {
          for (const candidate of directory.missing) if (await exists(candidate)) createdDirectories.add(candidate);
        }
        await removeCreatedDirectories(createdDirectories);
        throw new FileOpsError('FILE_OPS_APPLY_FAILED', 'Directory creation failed and the file transaction was rolled back', 'runtime', true, {
          error: error instanceof Error ? error.message : String(error),
          rolled_back: [...committed.backups.keys()].filter(file => !rollbackFailures.includes(file)),
          rollback_failures: rollbackFailures
        });
      }
    }

    const created = affected.filter(item => item.operation === 'add').map(item => String(item.path));
    const modified = affected.filter(item => item.operation === 'update').map(item => String(item.path));
    const deleted = affected.filter(item => item.operation === 'delete').map(item => String(item.path));
    return ok({
      dry_run: dryRun,
      preflight: true,
      applied: !dryRun,
      atomic: true,
      change_id: dryRun ? null : randomUUID().replaceAll('-', ''),
      diff,
      affected_files: affected,
      files_created: created,
      files_modified: modified,
      files_deleted: deleted,
      warnings: []
    });
  } catch (error) {
    if (error instanceof FileOpsError) return fail(error);
    throw error;
  }
}
