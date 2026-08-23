import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { realpath, stat } from 'node:fs/promises';
import path from 'node:path';
import type { ResolvedCommandSpec } from './policy.js';
import type { SandboxConfig, SandboxPathAccess, SandboxPathGrant } from './types.js';
import type { SandboxLaunch } from './sandbox.js';
import { isWslUncPath } from './wsl.js';

export const DOCKER_SBX_BACKEND_ID = 'docker_sbx';
export const DOCKER_SBX_DEFAULT_NETWORK = 'none';

const SANDBOX_NAME_PREFIX = 'ctmcp-';
const REMOTE_SUPERVISOR_SCRIPT = 'pidfile=$1; inner=$2; shift 2; setsid -w sh -c "$inner" ctmcp-inner "$pidfile" "$@"; status=$?; rm -f "$pidfile"; exit $status';
const REMOTE_INNER_SCRIPT = 'pidfile=$1; shift; printf \'%s\\n\' "$$" > "$pidfile"; exec "$@"';
const REMOTE_KILL_SCRIPT = 'pidfile=$1; i=0; while [ ! -s "$pidfile" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i+1)); done; [ -s "$pidfile" ] || exit 0; pid=$(cat "$pidfile") || exit 6; if kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null; then sleep 1; kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true; exit 0; fi; kill -0 "$pid" 2>/dev/null || exit 0; exit 5';

export interface DockerSbxMount {
  host: string;
  access: SandboxPathAccess;
}

export interface DockerSbxPreparedSandbox {
  cli: string;
  sandboxName: string;
  mounts: DockerSbxMount[];
}

interface CliResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

export class DockerSbxError extends Error {
  readonly code: string;
  readonly category: 'security' | 'runtime';
  readonly retryable = false;
  readonly details: Record<string, unknown>;

  constructor(code: string, message: string, stage: string, details: Record<string, unknown> = {}) {
    super(message);
    this.name = 'DockerSbxError';
    this.code = code;
    this.category = code.includes('PATH') || code.includes('COMMAND') || code.includes('MOUNT') ? 'security' : 'runtime';
    this.details = {
      sandbox_backend: DOCKER_SBX_BACKEND_ID,
      stage,
      fallback_allowed: false,
      ...details
    };
  }
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
  const normalized = path.resolve(stripVerbatimPrefix(value)).replaceAll('\\', '/').replace(/\/+$/, '');
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

function pathInside(parent: string, child: string): boolean {
  const normalizedParent = comparablePath(parent);
  const normalizedChild = comparablePath(child);
  return normalizedChild === normalizedParent || normalizedChild.startsWith(`${normalizedParent}/`);
}

async function canonicalDirectory(value: string, label: string): Promise<string> {
  if (isUnsupportedRemotePath(value)) {
    throw new DockerSbxError(
      'SANDBOX_SBX_PATH_UNSUPPORTED',
      `${label} cannot use a network-backed path: ${value}`,
      'mounts'
    );
  }
  let resolved: string;
  try {
    resolved = await realpath(value);
  } catch (error) {
    throw new DockerSbxError(
      'SANDBOX_SBX_PATH_INVALID',
      `${label} is unavailable: ${value}: ${error instanceof Error ? error.message : String(error)}`,
      'mounts'
    );
  }
  if (isUnsupportedRemotePath(resolved) || !(await stat(resolved)).isDirectory()) {
    throw new DockerSbxError(
      'SANDBOX_SBX_PATH_INVALID',
      `${label} must be an existing local directory: ${value}`,
      'mounts'
    );
  }
  return resolved;
}

export function discoverDockerSbxProgram(): string | undefined {
  if (process.platform !== 'win32') return undefined;
  const localAppData = process.env.LOCALAPPDATA;
  if (localAppData) {
    const candidate = path.join(localAppData, 'DockerSandboxes', 'bin', 'sbx.exe');
    if (requireExists(candidate)) return candidate;
  }
  try {
    const output = execFileSync('where.exe', ['sbx'], { encoding: 'utf8', windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
    const candidate = output.split(/\r?\n/).map(value => value.trim()).find(Boolean);
    return candidate || undefined;
  } catch {
    return undefined;
  }
}

function requireExists(value: string): boolean {
  return existsSync(value);
}

export function dockerSbxHostAvailable(): boolean {
  return discoverDockerSbxProgram() !== undefined;
}

export async function buildDockerSbxMounts(workspaceRoot: string, grants: SandboxPathGrant[]): Promise<DockerSbxMount[]> {
  const workspace = await canonicalDirectory(workspaceRoot, 'workspace root');
  const external = new Map<string, { host: string; access: SandboxPathAccess }>();
  for (const grant of grants) {
    const raw = grant.path.trim();
    if (!raw) continue;
    const host = await canonicalDirectory(raw, 'external sandbox path');
    if (pathInside(workspace, host)) continue;
    if (pathInside(host, workspace)) {
      throw new DockerSbxError(
        'SANDBOX_SBX_MOUNT_OVERLAP',
        `External sandbox path contains the primary workspace: ${raw}`,
        'mounts'
      );
    }
    const key = comparablePath(host);
    const current = external.get(key);
    external.set(key, {
      host,
      access: current?.access === 'modify' || grant.access === 'modify' ? 'modify' : 'read_only'
    });
  }
  const entries = [...external.values()].sort((left, right) => comparablePath(left.host).localeCompare(comparablePath(right.host)));
  for (const parent of entries) {
    if (parent.access !== 'modify') continue;
    for (const child of entries) {
      if (parent === child) continue;
      if (child.access === 'read_only' && pathInside(parent.host, child.host)) {
        throw new DockerSbxError(
          'SANDBOX_SBX_MOUNT_OVERLAP',
          `Writable external path contains a read-only grant: ${parent.host} contains ${child.host}`,
          'mounts'
        );
      }
    }
  }
  return [{ host: workspace, access: 'modify' }, ...entries];
}

function hostMountPath(value: string): string {
  return stripVerbatimPrefix(value);
}

function sandboxRuntimePath(value: string): string {
  const host = hostMountPath(value);
  if (/^[A-Za-z]:[\\/]/.test(host)) {
    const drive = host[0].toLowerCase();
    const rest = host.slice(3).replaceAll('\\', '/');
    return rest ? `/${drive}/${rest}` : `/${drive}`;
  }
  return host.replaceAll('\\', '/');
}

function windowsOnlyProgram(program: string): boolean {
  return /(?:\.exe|\.cmd|\.bat|\.ps1)$/i.test(path.basename(program));
}

function runCli(cli: string, args: string[], timeoutMs = 30_000): Promise<CliResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(cli, args, { windowsHide: true, shell: false, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try { child.kill('SIGKILL'); } catch { /* best effort */ }
      reject(new DockerSbxError('SANDBOX_SBX_TIMEOUT', `sbx ${args[0] ?? ''} did not finish within ${timeoutMs} ms`, 'cli'));
    }, Math.max(1, timeoutMs));
    timer.unref();
    child.stdout.on('data', chunk => stdout.push(Buffer.from(chunk)));
    child.stderr.on('data', chunk => stderr.push(Buffer.from(chunk)));
    child.once('error', error => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new DockerSbxError('SANDBOX_SBX_UNAVAILABLE', `Failed to execute Docker Sandboxes sbx CLI: ${error.message}`, 'cli'));
    });
    child.once('close', code => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ code, stdout: Buffer.concat(stdout).toString('utf8'), stderr: Buffer.concat(stderr).toString('utf8') });
    });
  });
}

function outputError(code: string, message: string, stage: string, result: CliResult, suggestion?: string): DockerSbxError {
  return new DockerSbxError(code, message, stage, {
    exit_code: result.code,
    stdout: result.stdout,
    stderr: result.stderr,
    ...(suggestion ? { suggestion } : {})
  });
}

function sandboxName(mounts: DockerSbxMount[]): string {
  const digest = createHash('sha256');
  digest.update('coding-tools-mcp/docker-sbx/v1\0');
  for (const mount of mounts) {
    digest.update(hostMountPath(mount.host));
    digest.update(mount.access === 'read_only' ? '\0ro\0' : '\0rw\0');
  }
  return `${SANDBOX_NAME_PREFIX}${digest.digest('hex').slice(0, 24)}`;
}

async function ensureSandbox(cli: string, name: string, mounts: DockerSbxMount[]): Promise<void> {
  const listed = await runCli(cli, ['ls', '--json']);
  if (listed.code !== 0) throw outputError('SANDBOX_SBX_LIST_FAILED', 'Unable to list Docker Sandboxes.', 'list', listed);
  let value: unknown;
  try { value = JSON.parse(listed.stdout); } catch (error) {
    throw new DockerSbxError('SANDBOX_SBX_LIST_FAILED', `Docker Sandboxes returned invalid JSON: ${error instanceof Error ? error.message : String(error)}`, 'list');
  }
  if (Array.isArray((value as { sandboxes?: unknown })?.sandboxes) && (value as { sandboxes: Array<{ name?: string; Name?: string }> }).sandboxes.some(item => (item.name ?? item.Name) === name)) return;
  const args = ['create', '-q', '--name', name, 'shell'];
  for (const mount of mounts) args.push(`${hostMountPath(mount.host)}${mount.access === 'read_only' ? ':ro' : ''}`);
  const created = await runCli(cli, args);
  if (created.code === 0) return;
  const lower = `${created.stdout}\n${created.stderr}`.toLowerCase();
  if (lower.includes('global network policy has not been initialized')) {
    throw outputError('SANDBOX_SBX_SETUP_REQUIRED', 'Docker Sandboxes network policy has not been initialized.', 'create', created, 'Choose a Docker Sandboxes network policy with sbx policy init.');
  }
  if (lower.includes('login') || lower.includes('sign in') || lower.includes('not authenticated')) {
    throw outputError('SANDBOX_SBX_SETUP_REQUIRED', 'Docker Sandboxes requires authentication before this sandbox can be created.', 'create', created, 'Run sbx login, then retry.');
  }
  throw outputError('SANDBOX_SBX_CREATE_FAILED', 'Docker Sandbox creation failed.', 'create', created);
}

async function ensureRemoteSupervisor(cli: string, name: string): Promise<void> {
  const result = await runCli(cli, ['exec', name, 'sh', '-c', 'command -v setsid >/dev/null 2>&1 && setsid -w true']);
  if (result.code === 0) return;
  throw outputError(
    'SANDBOX_SBX_SUPERVISOR_UNAVAILABLE',
    'Docker Sandbox does not provide the process-group supervisor required for reliable cancellation.',
    'supervisor',
    result,
    'Use a Docker Sandboxes shell environment that provides sh and setsid with -w support.'
  );
}

export async function prepareDockerSbx(config: SandboxConfig, workspaceRoot: string): Promise<DockerSbxPreparedSandbox> {
  if (process.platform !== 'win32') throw new DockerSbxError('SANDBOX_BACKEND_UNSUPPORTED', 'Docker Sandboxes are currently supported by the Node Agent only on Windows hosts.', 'prepare');
  const cli = discoverDockerSbxProgram();
  if (!cli) throw new DockerSbxError('SANDBOX_SBX_UNAVAILABLE', 'Docker Sandboxes sbx CLI was not found.', 'discovery');
  const mounts = await buildDockerSbxMounts(workspaceRoot, config.externalPaths);
  const name = sandboxName(mounts);
  await ensureSandbox(cli, name, mounts);
  await ensureRemoteSupervisor(cli, name);
  return { cli, sandboxName: name, mounts };
}

function pathForMount(mounts: DockerSbxMount[], hostPath: string): string | undefined {
  const match = mounts
    .filter(mount => pathInside(mount.host, hostPath))
    .sort((left, right) => comparablePath(right.host).length - comparablePath(left.host).length)[0];
  if (!match) return undefined;
  const relative = path.relative(stripVerbatimPrefix(match.host), stripVerbatimPrefix(hostPath)).replaceAll('\\', '/');
  const container = sandboxRuntimePath(match.host);
  return relative && relative !== '.' ? `${container}/${relative}` : container;
}

function commandEnvironment(environment: Array<[string, string]>, removeEnvironment: string[]): { values: Array<[string, string]>; removed: string[] } {
  const values = new Map(environment);
  const removed = new Set<string>();
  for (const key of removeEnvironment) {
    values.delete(key);
    removed.add(key);
  }
  return { values: [...values.entries()].sort(([left], [right]) => left.localeCompare(right)), removed: [...removed].sort() };
}

async function cancelRemoteProcess(cli: string, name: string, pidfile: string): Promise<void> {
  const result = await runCli(cli, ['exec', name, 'sh', '-c', REMOTE_KILL_SCRIPT, 'ctmcp-kill', pidfile], 10_000);
  if (result.code === 0) return;
  throw outputError('SANDBOX_SBX_CANCEL_FAILED', 'Docker Sandbox remote process cancellation failed.', 'cancel', result);
}

export function prepareDockerSbxLaunch(
  prepared: DockerSbxPreparedSandbox,
  cwd: string,
  spec: ResolvedCommandSpec,
  environment: Array<[string, string]>,
  removeEnvironment: string[]
): SandboxLaunch {
  const canonicalCwd = path.resolve(cwd);
  const containerCwd = pathForMount(prepared.mounts, canonicalCwd);
  if (!containerCwd) throw new DockerSbxError('SANDBOX_SBX_PATH_UNMOUNTED', `Command working directory is not mounted in the Docker sandbox: ${cwd}`, 'launch');
  if (windowsOnlyProgram(spec.program)) {
    throw new DockerSbxError('SANDBOX_SBX_COMMAND_UNSUPPORTED', `Windows host executable cannot run in the Docker Linux sandbox: ${spec.program}`, 'launch');
  }
  let program = spec.program;
  if (path.isAbsolute(program)) {
    const canonicalProgram = path.resolve(program);
    if (!pathForMount(prepared.mounts, canonicalProgram)) throw new DockerSbxError('SANDBOX_SBX_COMMAND_UNMOUNTED', `Sandbox command path is outside mounted workspaces: ${program}`, 'launch');
    program = pathForMount(prepared.mounts, canonicalProgram)!;
  } else if (program.includes('/') || program.includes('\\')) {
    throw new DockerSbxError('SANDBOX_SBX_COMMAND_UNAVAILABLE', `Relative sandbox command path was not resolved inside a mounted workspace: ${program}`, 'launch');
  }
  if (!program.trim()) throw new DockerSbxError('SANDBOX_PROCESS_PLAN_INVALID', 'Docker sandbox process plan has an empty command.', 'launch');
  const pidfile = `/tmp/ctmcp-${randomUUID()}.pid`;
  const effective = commandEnvironment(environment, removeEnvironment);
  const args = ['exec', '-i', '-w', containerCwd, prepared.sandboxName, 'env'];
  for (const key of effective.removed) args.push('-u', key);
  for (const [key, value] of effective.values) args.push(`${key}=${value}`);
  args.push('sh', '-c', REMOTE_SUPERVISOR_SCRIPT, 'ctmcp-supervisor', pidfile, REMOTE_INNER_SCRIPT, program, ...spec.argv);
  let cleanupPromise: Promise<void> | undefined;
  return {
    program: prepared.cli,
    args,
    environmentMode: 'forwarded',
    kill: () => cancelRemoteProcess(prepared.cli, prepared.sandboxName, pidfile),
    cleanup: () => {
      if (cleanupPromise) return cleanupPromise;
      cleanupPromise = Promise.resolve();
      return cleanupPromise;
    },
    processTreeContained: false,
    processTreeControl: 'sbx_supervised_process_group'
  };
}

export function dockerSbxRuntimePath(pathValue: string): string {
  return sandboxRuntimePath(pathValue);
}
