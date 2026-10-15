import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import { createInterface } from 'node:readline';
import { stat } from 'node:fs/promises';
import path from 'node:path';
import { binaryExcludeGlobs, fileLooksBinary, textSearchGlobs } from './textCodec.js';
import { DEFAULT_EXCLUDED_NAMES, relativeInside, resolveInside } from './workspace.js';

export type SearchBackend = 'rg' | 'grep';

export interface ExternalSearchQuery {
  query: string;
  regex: boolean;
  caseSensitive: boolean;
}

export interface ExternalMatchLine {
  path: string;
  line: number;
  text: string;
  before: string[];
  after: string[];
}

export interface ExternalSearchRequest {
  dataDir?: string;
  root: string;
  searchPath: string;
  isDirectory: boolean;
  queries: readonly ExternalSearchQuery[];
  includeGlobs: readonly string[];
  excludeGlobs: readonly string[];
  excludedPrefixes: readonly string[];
  includeHidden: boolean;
  includeIgnored: boolean;
  includeGenerated: boolean;
  restrictToText: boolean;
  contextLines: number;
  maxFileBytes: number;
  maxMatchesPerFile: number;
  ignoreFiles?: readonly string[];
  timeoutMs?: number;
}

export interface ExternalSearchStats {
  backend: SearchBackend;
  filesConsidered: number;
  scannedFiles: number;
  skippedBinaryFiles: number;
  skippedLargeFiles: number;
}

const DEFAULT_TIMEOUT_MS = 120_000;
const STDERR_LIMIT = 8 * 1024;

let resolvedBackend: { dataDir: string; result: { backend: SearchBackend; program: string } | null } | undefined;

export function managedRipgrepPath(dataDir?: string): string {
  const root = String(dataDir || process.env.CTMCP_DATA_DIR || '').trim();
  if (!root) return '';
  return path.join(root, 'bin', process.platform === 'win32' ? 'rg.exe' : 'rg');
}

function grepCandidates(): string[] {
  return process.platform === 'win32' ? ['grep.exe', 'grep'] : ['grep', '/usr/bin/grep'];
}

function requestedBackend(): 'rg' | 'grep' | 'js' | 'auto' {
  const value = String(process.env.CTMCP_SEARCH_BACKEND ?? 'auto').trim().toLowerCase();
  if (value === 'rg' || value === 'grep' || value === 'js') return value;
  return 'auto';
}

function commandWorks(program: string): boolean {
  try {
    const result = spawnSync(program, ['--version'], {
      encoding: 'utf8',
      timeout: 3_000,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'ignore']
    });
    return result.status === 0 && !result.error;
  } catch {
    return false;
  }
}

export function resolveGrepProgram(): string | null {
  return grepCandidates().find(commandWorks) ?? null;
}

export function resolveSearchBackend(dataDir?: string): { backend: SearchBackend; program: string } | null {
  const cacheKey = dataDir ?? '';
  if (resolvedBackend !== undefined && resolvedBackend.dataDir === cacheKey) return resolvedBackend.result;
  const requested = requestedBackend();
  if (requested === 'js') {
    resolvedBackend = { dataDir: cacheKey, result: null };
    return null;
  }
  const managed = managedRipgrepPath(dataDir);
  const homeLocal = process.env.HOME ? `${process.env.HOME}/.local/bin/rg` : undefined;
  const rgCandidates = [
    ...(managed ? [managed] : []),
    ...(process.platform === 'win32'
      ? ['rg.exe', 'rg']
      : ['rg', '/usr/bin/rg', '/usr/local/bin/rg', ...(homeLocal ? [homeLocal] : [])])
  ];
  if (requested === 'rg' || requested === 'auto') {
    const program = rgCandidates.find(commandWorks);
    if (program) {
      resolvedBackend = { dataDir: cacheKey, result: { backend: 'rg', program } };
      return resolvedBackend.result;
    }
    if (requested === 'rg') {
      resolvedBackend = { dataDir: cacheKey, result: null };
      return null;
    }
  }
  if (requested === 'grep' || requested === 'auto') {
    const program = resolveGrepProgram();
    if (program) {
      resolvedBackend = { dataDir: cacheKey, result: { backend: 'grep', program } };
      return resolvedBackend.result;
    }
  }
  resolvedBackend = { dataDir: cacheKey, result: null };
  return null;
}

export function resetSearchBackendCache(): void {
  resolvedBackend = undefined;
}

function generatedExcludeGlobs(): string[] {
  return [...DEFAULT_EXCLUDED_NAMES]
    .filter(name => name.toLowerCase() !== '.git')
    .map(name => `!${name}/**`);
}

export function buildRipgrepArgs(
  request: ExternalSearchRequest,
  queries: readonly ExternalSearchQuery[]
): string[] {
  const args = [
    '--json',
    '--no-config',
    '--no-mmap',
    '--threads', '1',
    '--crlf',
    '--max-depth', '20',
    '--max-filesize', String(request.maxFileBytes),
    '--no-require-git',
    '-g', '!.git/**',
    '-g', '!.GIT/**'
  ];
  if (request.includeHidden) args.push('--hidden');
  if (request.includeIgnored) args.push('--no-ignore');
  else {
    for (const file of request.ignoreFiles ?? []) args.push('--ignore-file', file);
  }
  if (!request.includeGenerated) {
    for (const glob of generatedExcludeGlobs()) args.push('-g', glob);
  }
  for (const prefix of request.excludedPrefixes) args.push('-g', `!${prefix}/**`);
  if (request.includeGlobs.length) {
    for (const glob of request.includeGlobs) args.push('-g', glob);
  }
  for (const glob of binaryExcludeGlobs()) args.push('-g', `!${glob}`);
  for (const glob of request.excludeGlobs) args.push('-g', `!${glob}`);
  if (request.contextLines > 0) args.push('-C', String(request.contextLines));
  if (Number.isFinite(request.maxMatchesPerFile) && request.maxMatchesPerFile < Number.MAX_SAFE_INTEGER) {
    args.push('-m', String(request.maxMatchesPerFile));
  }
  const first = queries[0];
  if (!first.regex) args.push('-F');
  if (!first.caseSensitive) args.push('-i');
  for (const query of queries) args.push('-e', query.query);
  args.push('--', request.searchPath);
  return args;
}

function grepIncludeFromGlob(glob: string): string {
  const normalized = glob.replaceAll('\\', '/');
  const slash = normalized.lastIndexOf('/');
  return slash >= 0 ? normalized.slice(slash + 1) : normalized;
}

export function buildGrepArgs(
  request: ExternalSearchRequest,
  queries: readonly ExternalSearchQuery[]
): string[] {
  const args = [
    '-n',
    '-H',
    '-I',
    '--binary-files=without-match',
    '--color=never',
    '--no-messages',
    '--line-buffered',
    '-Z'
  ];
  if (request.isDirectory) {
    args.push('-r');
    args.push('--exclude-dir=.git', '--exclude-dir=.GIT');
    if (!request.includeHidden) args.push('--exclude-dir=.*', '--exclude=.*');
    if (!request.includeGenerated) {
      for (const name of DEFAULT_EXCLUDED_NAMES) {
        if (name.toLowerCase() === '.git') continue;
        args.push(`--exclude-dir=${name}`);
      }
    }
  }
  if (request.includeGlobs.length) {
    const seen = new Set<string>();
    for (const glob of request.includeGlobs) {
      const include = grepIncludeFromGlob(glob);
      if (!include || seen.has(include)) continue;
      seen.add(include);
      args.push(`--include=${include}`);
    }
  } else if (request.restrictToText) {
    for (const glob of textSearchGlobs()) args.push(`--include=${glob}`);
  }
  for (const glob of binaryExcludeGlobs()) args.push(`--exclude=${glob}`);
  for (const glob of request.excludeGlobs) args.push(`--exclude=${grepIncludeFromGlob(glob)}`);
  if (request.contextLines > 0) args.push('-C', String(request.contextLines));
  if (Number.isFinite(request.maxMatchesPerFile) && request.maxMatchesPerFile < Number.MAX_SAFE_INTEGER) {
    args.push('-m', String(request.maxMatchesPerFile));
  }
  const first = queries[0];
  args.push(first.regex ? '-E' : '-F');
  if (!first.caseSensitive) args.push('-i');
  for (const query of queries) args.push('-e', query.query);
  args.push('--', request.searchPath);
  return args;
}

interface PendingLine {
  path: string;
  line: number;
  text: string;
  before: string[];
  after: string[];
}

class ContextAssembler {
  private before: string[] = [];
  private pending: PendingLine[] = [];
  currentPath = '';

  constructor(private readonly contextLines: number) {}

  reset(path: string, emit: (line: PendingLine) => void): void {
    this.flush(true, emit);
    this.currentPath = path;
    this.before = [];
    this.pending = [];
  }

  see(line: number, text: string, isMatch: boolean, emit: (line: PendingLine) => void): void {
    if (this.contextLines > 0) {
      for (const item of this.pending) {
        if (item.after.length < this.contextLines) item.after.push(text);
      }
      this.flush(false, emit);
    }
    if (isMatch) {
      this.pending.push({
        path: this.currentPath,
        line,
        text,
        before: this.contextLines > 0 ? [...this.before] : [],
        after: []
      });
      if (this.contextLines === 0) this.flush(true, emit);
    }
    if (this.contextLines > 0) {
      this.before.push(text);
      if (this.before.length > this.contextLines) this.before.shift();
    }
  }

  flush(force: boolean, emit: (line: PendingLine) => void): void {
    const kept: PendingLine[] = [];
    for (const item of this.pending) {
      if (!force && item.after.length < this.contextLines) {
        kept.push(item);
        continue;
      }
      emit(item);
    }
    this.pending = kept;
  }
}

function normalizeHitPath(root: string, raw: string): string | undefined {
  const trimmed = raw.replaceAll('\\', '/').replace(/^\.\//, '');
  if (!trimmed || trimmed === '.') return undefined;
  try {
    const full = path.isAbsolute(raw) ? raw : resolveInside(root, trimmed);
    return relativeInside(root, full).replaceAll('\\', '/') || undefined;
  } catch {
    return undefined;
  }
}

function decodeRgPath(value: unknown): string | undefined {
  if (!value || typeof value !== 'object') return undefined;
  const record = value as { text?: unknown; bytes?: unknown };
  if (typeof record.text === 'string') return record.text;
  return undefined;
}

function decodeRgLine(value: unknown): string {
  if (!value || typeof value !== 'object') return '';
  const record = value as { text?: unknown };
  return typeof record.text === 'string' ? record.text.replace(/\r?\n$/, '') : '';
}

async function runProcessLines(
  program: string,
  args: string[],
  cwd: string,
  onLine: (line: string) => boolean | Promise<boolean>,
  timeoutMs: number
): Promise<{ code: number | null; stderr: string; killed: boolean }> {
  let child: ChildProcess;
  try {
    child = spawn(program, args, {
      cwd,
      env: process.env,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe']
    });
  } catch {
    return { code: null, stderr: 'spawn failed', killed: false };
  }

  const stdout = child.stdout;
  const stderrStream = child.stderr;
  if (!stdout || !stderrStream) {
    child.kill('SIGKILL');
    return { code: null, stderr: 'search backend pipes missing', killed: true };
  }

  let stderr = '';
  let killed = false;
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    killed = true;
    child.kill('SIGTERM');
  }, timeoutMs);

  stderrStream.on('data', chunk => {
    if (stderr.length >= STDERR_LIMIT) return;
    stderr = (stderr + String(chunk)).slice(0, STDERR_LIMIT);
  });

  const stop = (): void => {
    if (killed) return;
    killed = true;
    child.kill('SIGTERM');
  };

  const rl = createInterface({ input: stdout, crlfDelay: Infinity });
  try {
    for await (const line of rl) {
      if (timedOut) break;
      const keepGoing = await onLine(line);
      if (!keepGoing) {
        stop();
        break;
      }
    }
  } finally {
    rl.close();
    clearTimeout(timer);
  }

  if (killed && child.exitCode === null && child.signalCode === null) {
    await new Promise<void>(resolve => {
      const killTimer = setTimeout(() => {
        child.kill('SIGKILL');
        resolve();
      }, 1_000);
      child.once('close', () => {
        clearTimeout(killTimer);
        resolve();
      });
    });
  }

  const code = await new Promise<number | null>(resolve => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolve(child.exitCode);
      return;
    }
    child.once('close', (exitCode) => resolve(exitCode));
  });

  if (timedOut) {
    throw new Error(`search backend timed out after ${timeoutMs}ms`);
  }
  return { code, stderr, killed };
}

function parseGrepLine(raw: string): { path: string; line: number; text: string; isMatch: boolean } | undefined {
  const nul = raw.indexOf('\0');
  if (nul < 0) return undefined;
  const file = raw.slice(0, nul);
  const rest = raw.slice(nul + 1);
  const match = /^(\d+)([:-])(.*)$/.exec(rest);
  if (!match) return undefined;
  return {
    path: file,
    line: Number(match[1]),
    isMatch: match[2] === ':',
    text: match[3]
  };
}

function groupQueries(queries: readonly ExternalSearchQuery[]): ExternalSearchQuery[][] {
  const groups = new Map<string, ExternalSearchQuery[]>();
  for (const query of queries) {
    const key = `${query.regex ? 1 : 0}:${query.caseSensitive ? 1 : 0}`;
    const group = groups.get(key);
    if (group) group.push(query);
    else groups.set(key, [query]);
  }
  return [...groups.values()];
}

export async function runExternalContentSearch(
  request: ExternalSearchRequest,
  onLine: (line: ExternalMatchLine, group: readonly ExternalSearchQuery[]) => boolean | Promise<boolean>
): Promise<ExternalSearchStats | null> {
  const resolved = resolveSearchBackend(request.dataDir);
  if (!resolved || request.queries.length === 0) return null;
  const stats: ExternalSearchStats = {
    backend: resolved.backend,
    filesConsidered: 0,
    scannedFiles: 0,
    skippedBinaryFiles: 0,
    skippedLargeFiles: 0
  };
  const inspected = new Map<string, 'ok' | 'binary' | 'large' | 'missing'>();
  let stop = false;

  const inspect = async (relative: string): Promise<'ok' | 'binary' | 'large' | 'missing'> => {
    const cached = inspected.get(relative);
    if (cached) return cached;
    const full = resolveInside(request.root, relative);
    let verdict: 'ok' | 'binary' | 'large' | 'missing' = 'ok';
    try {
      const info = await stat(full);
      stats.filesConsidered += 1;
      if (info.size > request.maxFileBytes) verdict = 'large';
      else if (await fileLooksBinary(full)) verdict = 'binary';
      else {
        stats.scannedFiles += 1;
        verdict = 'ok';
      }
    } catch {
      verdict = 'missing';
    }
    if (verdict === 'binary') stats.skippedBinaryFiles += 1;
    if (verdict === 'large') stats.skippedLargeFiles += 1;
    inspected.set(relative, verdict);
    return verdict;
  };

  const queued: PendingLine[] = [];
  const assembler = new ContextAssembler(request.contextLines);
  let activeGroup: readonly ExternalSearchQuery[] = request.queries;
  const queueEmit = (item: PendingLine): void => {
    queued.push(item);
  };
  const drain = async (): Promise<void> => {
    while (queued.length && !stop) {
      const item = queued.shift();
      if (!item) continue;
      const relative = normalizeHitPath(request.root, item.path);
      if (!relative) continue;
      const verdict = await inspect(relative);
      if (verdict !== 'ok') continue;
      const keepGoing = await onLine({
        path: relative,
        line: item.line,
        text: item.text,
        before: item.before,
        after: item.after
      }, activeGroup);
      if (!keepGoing) stop = true;
    }
  };

  const timeoutMs = Math.max(1_000, request.timeoutMs ?? DEFAULT_TIMEOUT_MS);
  const groups = groupQueries(request.queries);

  for (const group of groups) {
    if (stop) break;
    activeGroup = group;
    const args = resolved.backend === 'rg'
      ? buildRipgrepArgs(request, group)
      : buildGrepArgs(request, group);
    const handleLine = async (raw: string): Promise<boolean> => {
      if (stop) return false;
      if (resolved.backend === 'rg') {
        let event: { type?: string; data?: Record<string, unknown> };
        try { event = JSON.parse(raw) as { type?: string; data?: Record<string, unknown> }; }
        catch { return true; }
        const type = event.type;
        const data = event.data ?? {};
        if (type === 'begin') {
          const nextPath = decodeRgPath(data.path);
          if (nextPath) assembler.reset(nextPath, queueEmit);
          await drain();
          return true;
        }
        if (type === 'end') {
          assembler.flush(true, queueEmit);
          await drain();
          return !stop;
        }
        if (type === 'match' || type === 'context') {
          const nextPath = decodeRgPath(data.path);
          const text = decodeRgLine(data.lines);
          const lineNumber = Number(data.line_number);
          if (!nextPath || !Number.isFinite(lineNumber) || lineNumber < 1) return true;
          if (assembler.currentPath !== nextPath) assembler.reset(nextPath, queueEmit);
          assembler.see(lineNumber, text, type === 'match', queueEmit);
          await drain();
        }
        return !stop;
      }
      const parsed = parseGrepLine(raw);
      if (!parsed) return true;
      if (assembler.currentPath !== parsed.path) assembler.reset(parsed.path, queueEmit);
      assembler.see(parsed.line, parsed.text, parsed.isMatch, queueEmit);
      await drain();
      return !stop;
    };

    const result = await runProcessLines(resolved.program, args, request.root, handleLine, timeoutMs);
    assembler.flush(true, queueEmit);
    await drain();
    if (!result.killed && result.code === 2 && result.stderr.trim()) {
      throw new Error(result.stderr.trim());
    }
  }

  return stats;
}
