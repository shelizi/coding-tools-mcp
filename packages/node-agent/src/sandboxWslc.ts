import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { access, realpath, stat } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import type { ResolvedCommandSpec } from './policy.js';
import type { SandboxConfig, SandboxPathAccess, SandboxPathGrant } from './types.js';
import { ensureWslcSessionStorage } from './sandboxWslcProvisioner.js';
import {
  acquireWslcStorageProcessLock,
  WSLC_STORAGE_ADMISSION_MAX_MS
} from './sandboxWslcProcessLock.js';
import { isWslUncPath } from './wsl.js';

export const WSLC_BACKEND_ID = 'wslc';
export const WSLC_DEFAULT_IMAGE = 'coding-tools-mcp/wslc-sandbox:alpine-3.21';
export const WSLC_DEFAULT_NETWORK = 'none';
export const WSLC_SESSION_STORAGE_OPTION = 'wslc.session_storage';
const WSLC_BUILTIN_IMAGE_CONTEXT = fileURLToPath(new URL('./sandbox/wslc/', import.meta.url));

export interface WslcMount {
  host: string;
  container: string;
  access: SandboxPathAccess;
}

export interface WslcPreparedSandbox {
  cli: string;
  image: string;
  network: string;
  mounts: WslcMount[];
  session: WslcSessionOwner;
}

export interface WslcLaunch {
  program: string;
  args: string[];
  containerName: string;
  environmentMode: 'forwarded';
  kill: () => Promise<void>;
  cleanup: () => Promise<void>;
  processTreeContained: true;
  processTreeControl: 'wslc_container';
}

export class WslcSandboxError extends Error {
  readonly code: string;
  readonly category: 'security' | 'runtime';
  readonly retryable: boolean;
  readonly details: Record<string, unknown>;

  constructor(
    code: string,
    message: string,
    stage: string,
    category: 'security' | 'runtime' = 'security',
    details: Record<string, unknown> = {},
    retryable = false
  ) {
    super(message);
    this.name = 'WslcSandboxError';
    this.code = code;
    this.category = category;
    this.retryable = retryable;
    this.details = {
      sandbox_backend: WSLC_BACKEND_ID,
      stage,
      fallback_allowed: false,
      ...details
    };
  }
}

interface CliResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

interface WslcSessionOwner {
  name: string;
  storage: string;
  close: () => Promise<void>;
}

interface WslcStorageQueue {
  active: boolean;
  waiters: Array<{
    resolve: () => void;
    reject: (error: Error) => void;
    signal?: AbortSignal;
    onAbort?: () => void;
    timer?: NodeJS.Timeout;
  }>;
}

const storageQueues = new Map<string, WslcStorageQueue>();

function sleep(ms: number): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, ms));
}

function queueCancelledError(): WslcSandboxError {
  return new WslcSandboxError(
    'SANDBOX_WSLC_QUEUE_CANCELLED',
    'WSLC storage admission was cancelled before a session could start.',
    'session_queue',
    'runtime'
  );
}

function advanceStorageQueue(key: string, queue: WslcStorageQueue): void {
  const next = queue.waiters.shift();
  if (next) {
    if (next.signal && next.onAbort) next.signal.removeEventListener('abort', next.onAbort);
    if (next.timer) clearTimeout(next.timer);
    next.resolve();
    return;
  }
  queue.active = false;
  storageQueues.delete(key);
}

async function acquireStorageLease(
  storage: string,
  signal?: AbortSignal,
  timeoutMs = WSLC_STORAGE_ADMISSION_MAX_MS
): Promise<() => void> {
  if (signal?.aborted) throw queueCancelledError();
  const boundedTimeoutMs = Math.max(1, Math.min(WSLC_STORAGE_ADMISSION_MAX_MS, Math.trunc(timeoutMs)));
  const key = comparablePath(storage);
  let queue = storageQueues.get(key);
  if (!queue) {
    queue = { active: false, waiters: [] };
    storageQueues.set(key, queue);
  }

  if (queue.active) {
    await new Promise<void>((resolve, reject) => {
      const waiter: WslcStorageQueue['waiters'][number] = { resolve, reject, signal };
      waiter.timer = setTimeout(() => {
        const index = queue!.waiters.indexOf(waiter);
        if (index >= 0) queue!.waiters.splice(index, 1);
        if (signal && waiter.onAbort) signal.removeEventListener('abort', waiter.onAbort);
        reject(new WslcSandboxError(
          'SANDBOX_WSLC_QUEUE_TIMEOUT',
          `Timed out after ${boundedTimeoutMs} ms waiting for the in-process WSLC storage queue.`,
          'session_queue',
          'runtime',
          { storage_queue_wait_ms: boundedTimeoutMs, timeout_ms: boundedTimeoutMs },
          true
        ));
      }, boundedTimeoutMs);
      waiter.timer.unref();
      if (signal) {
        waiter.onAbort = () => {
          const index = queue!.waiters.indexOf(waiter);
          if (index >= 0) queue!.waiters.splice(index, 1);
          if (waiter.timer) clearTimeout(waiter.timer);
          reject(queueCancelledError());
        };
        signal.addEventListener('abort', waiter.onAbort, { once: true });
      }
      queue!.waiters.push(waiter);
    });
    if (signal?.aborted) {
      advanceStorageQueue(key, queue);
      throw queueCancelledError();
    }
  }

  queue.active = true;
  let released = false;
  return () => {
    if (released) return;
    released = true;
    advanceStorageQueue(key, queue!);
  };
}

async function runCli(cli: string, args: string[], timeoutMs = 30_000): Promise<CliResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(cli, args, {
      windowsHide: true,
      shell: false,
      stdio: ['ignore', 'pipe', 'pipe']
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      try { child.kill('SIGKILL'); } catch { /* best effort */ }
      settled = true;
      reject(new WslcSandboxError(
        'SANDBOX_WSLC_TIMEOUT',
        `wslc ${args[0] ?? ''} did not finish within ${timeoutMs} ms`,
        'cli',
        'runtime'
      ));
    }, Math.max(1, timeoutMs));
    timer.unref();
    child.stdout.on('data', chunk => stdout.push(Buffer.from(chunk)));
    child.stderr.on('data', chunk => stderr.push(Buffer.from(chunk)));
    child.once('error', error => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new WslcSandboxError(
        'SANDBOX_WSLC_UNAVAILABLE',
        `Failed to execute wslc CLI: ${error.message}`,
        'cli',
        'runtime'
      ));
    });
    child.once('close', code => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({
        code,
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: Buffer.concat(stderr).toString('utf8')
      });
    });
  });
}

async function pathExists(value: string): Promise<boolean> {
  try {
    await access(value);
    return true;
  } catch {
    return false;
  }
}

export async function discoverWslcProgram(): Promise<string> {
  if (process.platform === 'win32' && process.env.ProgramFiles) {
    const candidate = path.join(process.env.ProgramFiles, 'WSL', 'wslc.exe');
    if (await pathExists(candidate)) return candidate;
  }
  return 'wslc';
}

export function selectedWslcImage(config: SandboxConfig): string {
  const image = String(config.options['wslc.image'] ?? WSLC_DEFAULT_IMAGE).trim() || WSLC_DEFAULT_IMAGE;
  if (/\s|[\u0000-\u001f\u007f]/.test(image)) {
    throw new WslcSandboxError(
      'SANDBOX_WSLC_IMAGE_INVALID',
      'WSLC container image contains whitespace or control characters.',
      'image'
    );
  }
  return image;
}

export function selectedWslcNetwork(config: SandboxConfig): string {
  const network = String(config.options['wslc.network'] ?? WSLC_DEFAULT_NETWORK).trim() || WSLC_DEFAULT_NETWORK;
  if (!/^[A-Za-z0-9_.-]+$/.test(network)) {
    throw new WslcSandboxError(
      'SANDBOX_WSLC_NETWORK_INVALID',
      "WSLC network must be 'none', 'bridge', or a simple named network.",
      'network'
    );
  }
  return network;
}

export function configuredWslcSessionStorage(config: SandboxConfig): string | undefined {
  const storage = String(config.options[WSLC_SESSION_STORAGE_OPTION] ?? '').trim();
  return storage || undefined;
}

function stripVerbatimPrefix(value: string): string {
  if (/^\\\\\?\\UNC\\/i.test(value)) return `\\\\${value.slice(8)}`;
  if (/^\\\\\?\\/.test(value)) return value.slice(4);
  return value;
}

function isNetworkPath(value: string): boolean {
  const normalized = value.replaceAll('/', '\\');
  if (/^\\\\\?\\UNC\\/i.test(normalized)) return true;
  if (/^\\\\\?\\/i.test(normalized)) return false;
  return normalized.startsWith('\\\\');
}

function isUnsupportedRemotePath(value: string): boolean {
  return isNetworkPath(value) && !isWslUncPath(value);
}

function comparablePath(value: string): string {
  const stripped = stripVerbatimPrefix(value);
  const normalized = path.resolve(stripped).replaceAll('\\', '/').replace(/\/+$/, '');
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

function pathInside(parent: string, child: string): boolean {
  const normalizedParent = comparablePath(parent);
  const normalizedChild = comparablePath(child);
  return normalizedChild === normalizedParent || normalizedChild.startsWith(`${normalizedParent}/`);
}

async function canonicalDirectory(value: string, label: string): Promise<string> {
  if (isUnsupportedRemotePath(value)) {
    throw new WslcSandboxError(
      'SANDBOX_WSLC_PATH_UNSUPPORTED',
      `${label} cannot use a network-backed path: ${value}`,
      'mounts'
    );
  }
  let resolved: string;
  try {
    resolved = await realpath(value);
  } catch (error) {
    throw new WslcSandboxError(
      'SANDBOX_WSLC_PATH_INVALID',
      `${label} is unavailable: ${value}: ${error instanceof Error ? error.message : String(error)}`,
      'mounts'
    );
  }
  if (isUnsupportedRemotePath(resolved) || !(await stat(resolved)).isDirectory()) {
    throw new WslcSandboxError(
      'SANDBOX_WSLC_PATH_INVALID',
      `${label} must be an existing local directory: ${value}`,
      'mounts'
    );
  }
  return resolved;
}

export async function buildWslcMounts(workspaceRoot: string, grants: SandboxPathGrant[]): Promise<WslcMount[]> {
  const workspace = await canonicalDirectory(workspaceRoot, 'workspace root');
  const external = new Map<string, { host: string; access: SandboxPathAccess }>();
  for (const grant of grants) {
    const raw = grant.path.trim();
    if (!raw) continue;
    const host = await canonicalDirectory(raw, 'external sandbox path');
    if (pathInside(workspace, host)) continue;
    if (pathInside(host, workspace)) {
      throw new WslcSandboxError(
        'SANDBOX_WSLC_MOUNT_OVERLAP',
        `External sandbox path contains the primary workspace: ${raw}`,
        'mounts'
      );
    }
    const key = comparablePath(host);
    const existing = external.get(key);
    external.set(key, {
      host,
      access: existing?.access === 'modify' || grant.access === 'modify' ? 'modify' : 'read_only'
    });
  }
  const entries = [...external.values()].sort((left, right) => comparablePath(left.host).localeCompare(comparablePath(right.host)));
  for (const parent of entries) {
    if (parent.access !== 'modify') continue;
    for (const child of entries) {
      if (parent === child) continue;
      if (child.access === 'read_only' && pathInside(parent.host, child.host)) {
        throw new WslcSandboxError(
          'SANDBOX_WSLC_MOUNT_OVERLAP',
          `Writable external path contains a read-only grant: ${parent.host} contains ${child.host}`,
          'mounts'
        );
      }
    }
  }
  return [
    { host: workspace, container: '/workspace', access: 'modify' },
    ...entries.map((entry, index) => ({
      ...entry,
      container: `/ctmcp/grants/${index}`
    }))
  ];
}

export function containerPathForHost(mounts: WslcMount[], hostPath: string): string | undefined {
  const matches = mounts
    .filter(mount => pathInside(mount.host, hostPath))
    .sort((left, right) => comparablePath(right.host).length - comparablePath(left.host).length);
  const mount = matches[0];
  if (!mount) return undefined;
  const base = stripVerbatimPrefix(mount.host);
  const candidate = stripVerbatimPrefix(hostPath);
  const relative = path.relative(base, candidate).replaceAll('\\', '/');
  return relative && relative !== '.'
    ? `${mount.container.replace(/\/$/, '')}/${relative}`
    : mount.container;
}

function hostMountPath(value: string): string {
  return stripVerbatimPrefix(value);
}

async function ensureWslcReady(cli: string): Promise<void> {
  const result = await runCli(cli, ['version']);
  if (result.code === 0) return;
  throw new WslcSandboxError(
    'SANDBOX_WSLC_UNAVAILABLE',
    'wslc version failed.',
    'discovery',
    'runtime',
    { exit_code: result.code, stdout: result.stdout, stderr: result.stderr }
  );
}

function sessionArgs(sessionName: string, args: string[]): string[] {
  return ['--session', sessionName, ...args];
}

async function runSessionCli(cli: string, sessionName: string, args: string[], timeoutMs = 30_000): Promise<CliResult> {
  return runCli(cli, sessionArgs(sessionName, args), timeoutMs);
}

function sessionMissing(result: CliResult): boolean {
  const message = `${result.stdout}\n${result.stderr}`.toLowerCase();
  return /not found|no such|does not exist|not exist|cannot find|找不到/.test(message);
}

async function waitForOwnerExit(child: ChildProcessWithoutNullStreams, timeoutMs: number): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  await Promise.race([
    new Promise<void>(resolve => child.once('exit', () => resolve())),
    sleep(timeoutMs)
  ]);
}

async function openWslcSession(
  cli: string,
  storagePath: string,
  signal?: AbortSignal,
  admissionTimeoutMs = WSLC_STORAGE_ADMISSION_MAX_MS
): Promise<WslcSessionOwner> {
  const storage = await canonicalDirectory(storagePath, 'WSLC session storage');
  const releaseStorage = await acquireStorageLease(storage, signal, admissionTimeoutMs);
  let processLock;
  try {
    processLock = await acquireWslcStorageProcessLock(storage, signal, admissionTimeoutMs);
  } catch (error) {
    releaseStorage();
    throw error;
  }
  const name = `ctmcp-node-session-${randomUUID().replaceAll('-', '')}`;
  const owner = spawn(cli, ['system', 'session', 'enter', '--name', name, storage], {
    windowsHide: true,
    shell: false,
    stdio: ['pipe', 'pipe', 'pipe']
  });
  const stderr: Buffer[] = [];
  let ownerError: Error | undefined;
  owner.stderr.on('data', chunk => {
    if (Buffer.concat(stderr).length < 16_384) stderr.push(Buffer.from(chunk));
  });
  owner.stdout.resume();
  owner.once('error', error => { ownerError = error; });

  try {
    const started = Date.now();
    let ready = false;
    while (Date.now() - started < 20_000) {
      if (ownerError) throw ownerError;
      if (owner.exitCode !== null || owner.signalCode !== null) {
        throw new Error(`session owner exited before readiness (exit ${owner.exitCode ?? owner.signalCode ?? 'unknown'})`);
      }
      const listed = await runCli(cli, ['system', 'session', 'list'], 10_000);
      if (listed.code === 0 && listed.stdout.includes(name)) {
        ready = true;
        break;
      }
      await sleep(100);
    }
    if (!ready) throw new Error('session did not become ready within 20 seconds');
  } catch (error) {
    try { owner.stdin.end(); } catch { /* best effort */ }
    if (owner.exitCode === null && owner.signalCode === null) owner.kill('SIGKILL');
    await processLock.release();
    releaseStorage();
    throw new WslcSandboxError(
      'SANDBOX_WSLC_SESSION_OPEN_FAILED',
      `Failed to enter pre-provisioned WSLC session storage ${storage}: ${error instanceof Error ? error.message : String(error)}`,
      'session_storage',
      'runtime',
      { stderr: Buffer.concat(stderr).toString('utf8').trim() }
    );
  }

  let closePromise: Promise<void> | undefined;
  const close = (): Promise<void> => {
    if (closePromise) return closePromise;
    closePromise = (async () => {
      try {
        const terminated = await runSessionCli(cli, name, ['system', 'session', 'terminate'], 10_000);
        if (terminated.code !== 0 && !sessionMissing(terminated)) {
          throw new WslcSandboxError(
            'SANDBOX_WSLC_SESSION_TERMINATE_FAILED',
            `Failed to terminate WSLC session ${name}.`,
            'session_cleanup',
            'runtime',
            { exit_code: terminated.code, stdout: terminated.stdout, stderr: terminated.stderr }
          );
        }
      } finally {
        try { owner.stdin.end(); } catch { /* best effort */ }
        await waitForOwnerExit(owner, 2_000);
        if (owner.exitCode === null && owner.signalCode === null) owner.kill('SIGKILL');
        await processLock.release();
        releaseStorage();
      }
    })();
    return closePromise;
  };
  return { name, storage, close };
}

async function ensureWslcImage(cli: string, sessionName: string, image: string): Promise<void> {
  const inspected = await runSessionCli(cli, sessionName, ['image', 'inspect', image]);
  if (inspected.code === 0) return;
  if (image === WSLC_DEFAULT_IMAGE) {
    const dockerfile = path.join(WSLC_BUILTIN_IMAGE_CONTEXT, 'Dockerfile');
    if (!(await pathExists(dockerfile))) {
      throw new WslcSandboxError(
        'SANDBOX_WSLC_IMAGE_ASSET_MISSING',
        `Built-in WSLC sandbox Dockerfile is missing: ${dockerfile}`,
        'image',
        'runtime'
      );
    }
    const built = await runSessionCli(
      cli,
      sessionName,
      ['build', '--pull', '-t', image, WSLC_BUILTIN_IMAGE_CONTEXT],
      600_000
    );
    if (built.code === 0) return;
    throw new WslcSandboxError(
      'SANDBOX_WSLC_IMAGE_BUILD_FAILED',
      'WSLC could not build the built-in Alpine sandbox image.',
      'image',
      'runtime',
      { exit_code: built.code, stdout: built.stdout, stderr: built.stderr }
    );
  }
  const pulled = await runSessionCli(cli, sessionName, ['pull', image], 300_000);
  if (pulled.code === 0) return;
  throw new WslcSandboxError(
    'SANDBOX_WSLC_IMAGE_UNAVAILABLE',
    'WSLC could not inspect or pull the configured container image.',
    'image',
    'runtime',
    { exit_code: pulled.code, stdout: pulled.stdout, stderr: pulled.stderr }
  );
}

export async function prepareWslc(
  config: SandboxConfig,
  workspaceRoot: string,
  dataDir: string,
  signal?: AbortSignal,
  admissionTimeoutMs = WSLC_STORAGE_ADMISSION_MAX_MS
): Promise<WslcPreparedSandbox> {
  if (process.platform !== 'win32') {
    throw new WslcSandboxError(
      'SANDBOX_BACKEND_UNSUPPORTED',
      'WSL Containers are supported by the Node Agent only on Windows hosts.',
      'prepare'
    );
  }
  const cli = await discoverWslcProgram();
  const image = selectedWslcImage(config);
  const network = selectedWslcNetwork(config);
  await ensureWslcReady(cli);
  const mounts = await buildWslcMounts(workspaceRoot, config.externalPaths);
  const storage = await ensureWslcSessionStorage(
    configuredWslcSessionStorage(config),
    dataDir,
    workspaceRoot
  );
  const session = await openWslcSession(cli, storage, signal, admissionTimeoutMs);
  try {
    await ensureWslcImage(cli, session.name, image);
    return { cli, image, network, mounts, session };
  } catch (error) {
    try { await session.close(); } catch { /* preserve primary prepare error */ }
    throw error;
  }
}

export async function disposeWslc(prepared: WslcPreparedSandbox): Promise<void> {
  await prepared.session.close();
}

function windowsHostProgram(program: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(program) || /^\\\\\?\\/.test(program) || /^\\\\/.test(program);
}

export function buildWslcRunArgs(
  prepared: WslcPreparedSandbox,
  name: string,
  cwd: string,
  spec: ResolvedCommandSpec,
  environment: Array<[string, string]>,
  removeEnvironment: string[]
): string[] {
  const containerCwd = containerPathForHost(prepared.mounts, cwd);
  if (!containerCwd) {
    throw new WslcSandboxError(
      'SANDBOX_WSLC_PATH_UNMOUNTED',
      `Command working directory is not mounted in the WSLC container: ${cwd}`,
      'launch'
    );
  }
  let program = spec.program;
  if (windowsHostProgram(program)) {
    const mapped = containerPathForHost(prepared.mounts, program);
    if (!mapped) {
      throw new WslcSandboxError(
        'SANDBOX_WSLC_COMMAND_UNMOUNTED',
        `Sandbox command path is not mounted: ${program}`,
        'launch'
      );
    }
    program = mapped;
  }
  const effective = new Map(environment);
  const removed = new Set(removeEnvironment);
  for (const key of removed) effective.delete(key);

  const args = ['run', '--rm', '-i', '--name', name, '--network', prepared.network];
  for (const mount of prepared.mounts) {
    args.push('-v');
    const suffix = mount.access === 'read_only' ? ':ro' : '';
    args.push(`${hostMountPath(mount.host)}:${mount.container}${suffix}`);
  }
  args.push('-w', containerCwd);
  for (const [key, value] of effective) args.push('-e', `${key}=${value}`);
  args.push(prepared.image);
  if (removed.size) {
    args.push('env');
    for (const key of [...removed].sort()) args.push('-u', key);
  }
  args.push(program, ...spec.argv);
  return args;
}

function containerMissing(result: CliResult): boolean {
  const message = `${result.stdout}\n${result.stderr}`.toLowerCase();
  return /not found|no such|does not exist|not exist/.test(message);
}

export async function cancelWslcContainer(cli: string, sessionName: string, name: string): Promise<void> {
  let last: CliResult | undefined;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const result = await runSessionCli(cli, sessionName, ['remove', '-f', name], 10_000);
    if (result.code === 0 || containerMissing(result)) return;
    last = result;
    await sleep(50);
  }
  throw new WslcSandboxError(
    'SANDBOX_WSLC_CANCEL_FAILED',
    `Failed to remove WSLC container ${name}.`,
    'cancel',
    'runtime',
    { exit_code: last?.code ?? null, stdout: last?.stdout ?? '', stderr: last?.stderr ?? '' }
  );
}

export function prepareWslcLaunch(
  prepared: WslcPreparedSandbox,
  cwd: string,
  spec: ResolvedCommandSpec,
  environment: Array<[string, string]>,
  removeEnvironment: string[]
): WslcLaunch {
  const containerName = `ctmcp-node-wslc-${randomUUID().replaceAll('-', '')}`;
  const args = sessionArgs(
    prepared.session.name,
    buildWslcRunArgs(prepared, containerName, cwd, spec, environment, removeEnvironment)
  );
  const kill = () => cancelWslcContainer(prepared.cli, prepared.session.name, containerName);
  let cleanupPromise: Promise<void> | undefined;
  return {
    program: prepared.cli,
    args,
    containerName,
    environmentMode: 'forwarded',
    kill,
    cleanup: () => {
      if (cleanupPromise) return cleanupPromise;
      cleanupPromise = (async () => {
        try { await kill(); } catch { /* best-effort cleanup after normal exit */ }
        await prepared.session.close();
      })();
      return cleanupPromise;
    },
    processTreeContained: true,
    processTreeControl: 'wslc_container'
  };
}
