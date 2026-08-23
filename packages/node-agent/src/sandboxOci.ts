import { execFileSync, spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { realpath, stat } from 'node:fs/promises';
import path from 'node:path';
import type { ResolvedCommandSpec } from './policy.js';
import type { SandboxConfig, SandboxPathAccess, SandboxPathGrant } from './types.js';
import type { SandboxLaunch } from './sandbox.js';
import { isWslUncPath } from './wsl.js';

export type OciRuntime = 'docker' | 'podman';

export const DOCKER_BACKEND_ID = 'docker';
export const PODMAN_BACKEND_ID = 'podman';
export const OCI_DEFAULT_IMAGE = 'ubuntu:24.04';
export const OCI_DEFAULT_NETWORK = 'none';

export interface OciMount {
  host: string;
  container: string;
  access: SandboxPathAccess;
}

export interface OciPreparedSandbox {
  runtime: OciRuntime;
  cli: string;
  image: string;
  network: string;
  mounts: OciMount[];
}

interface CliResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

export class OciSandboxError extends Error {
  readonly code: string;
  readonly category: 'security' | 'runtime';
  readonly retryable = false;
  readonly details: Record<string, unknown>;

  constructor(runtime: OciRuntime, code: string, message: string, stage: string, details: Record<string, unknown> = {}) {
    super(message);
    this.name = 'OciSandboxError';
    this.code = code;
    this.category = code.includes('PATH') || code.includes('COMMAND') || code.includes('MOUNT') || code.includes('NETWORK') || code.includes('IMAGE_INVALID')
      ? 'security'
      : 'runtime';
    this.details = {
      sandbox_backend: runtime,
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

async function canonicalDirectory(runtime: OciRuntime, value: string, label: string): Promise<string> {
  if (isUnsupportedRemotePath(value)) {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_PATH_UNSUPPORTED',
      `${label} cannot use a network-backed path: ${value}`,
      'mounts'
    );
  }
  let resolved: string;
  try {
    resolved = await realpath(value);
  } catch (error) {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_PATH_INVALID',
      `${label} is unavailable: ${value}: ${error instanceof Error ? error.message : String(error)}`,
      'mounts'
    );
  }
  if (isUnsupportedRemotePath(resolved) || !(await stat(resolved)).isDirectory()) {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_PATH_INVALID',
      `${label} must be an existing local directory: ${value}`,
      'mounts'
    );
  }
  return resolved;
}

function lookupOnPath(name: string): string | undefined {
  try {
    const command = process.platform === 'win32' ? 'where.exe' : 'which';
    const output = execFileSync(command, [name], {
      encoding: 'utf8',
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'ignore']
    });
    return output.split(/\r?\n/).map(value => value.trim()).find(Boolean);
  } catch {
    return undefined;
  }
}

function wellKnownWindowsCli(runtime: OciRuntime): string | undefined {
  if (process.platform !== 'win32') return undefined;
  const programFiles = process.env.ProgramFiles;
  if (!programFiles) return undefined;
  const candidate = runtime === 'docker'
    ? path.join(programFiles, 'Docker', 'Docker', 'resources', 'bin', 'docker.exe')
    : path.join(programFiles, 'RedHat', 'Podman', 'podman.exe');
  return existsSync(candidate) ? candidate : undefined;
}

export function discoverOciProgram(runtime: OciRuntime): string | undefined {
  return lookupOnPath(runtime) ?? wellKnownWindowsCli(runtime);
}

export function discoverDockerProgram(): string | undefined {
  return discoverOciProgram('docker');
}

export function discoverPodmanProgram(): string | undefined {
  return discoverOciProgram('podman');
}

export function dockerHostAvailable(): boolean {
  return discoverDockerProgram() !== undefined;
}

export function podmanHostAvailable(): boolean {
  return discoverPodmanProgram() !== undefined;
}

export function selectedOciImage(runtime: OciRuntime, config: SandboxConfig): string {
  const key = runtime === 'docker' ? 'docker.image' : 'podman.image';
  const image = String(config.options[key] ?? OCI_DEFAULT_IMAGE).trim() || OCI_DEFAULT_IMAGE;
  if (/[\s\u0000-\u001f]/.test(image)) {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_IMAGE_INVALID',
      `${runtimeLabel(runtime)} container image contains whitespace or control characters.`,
      'image'
    );
  }
  return image;
}

export function selectedOciNetwork(runtime: OciRuntime, config: SandboxConfig): string {
  const key = runtime === 'docker' ? 'docker.network' : 'podman.network';
  const network = String(config.options[key] ?? OCI_DEFAULT_NETWORK).trim() || OCI_DEFAULT_NETWORK;
  if (!/^[A-Za-z0-9_.-]+$/.test(network)) {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_NETWORK_INVALID',
      `${runtimeLabel(runtime)} network name contains unsupported characters.`,
      'network'
    );
  }
  if (network.toLowerCase() === 'host') {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_NETWORK_FORBIDDEN',
      `${runtimeLabel(runtime)} host networking is not allowed because it escapes the container network namespace.`,
      'network'
    );
  }
  return network;
}

export async function buildOciMounts(
  runtime: OciRuntime,
  workspaceRoot: string,
  grants: SandboxPathGrant[]
): Promise<OciMount[]> {
  const workspace = await canonicalDirectory(runtime, workspaceRoot, 'workspace root');
  const external = new Map<string, { host: string; access: SandboxPathAccess }>();
  for (const grant of grants) {
    const raw = grant.path.trim();
    if (!raw) continue;
    const host = await canonicalDirectory(runtime, raw, 'external sandbox path');
    if (pathInside(workspace, host)) continue;
    if (pathInside(host, workspace)) {
      throw new OciSandboxError(
        runtime,
        'SANDBOX_OCI_MOUNT_OVERLAP',
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
        throw new OciSandboxError(
          runtime,
          'SANDBOX_OCI_MOUNT_OVERLAP',
          `Writable external path contains a read-only grant: ${parent.host} contains ${child.host}`,
          'mounts'
        );
      }
    }
  }
  return [
    { host: workspace, container: '/workspace', access: 'modify' },
    ...entries.map((entry, index) => ({
      host: entry.host,
      container: `/ctmcp/grants/${index}`,
      access: entry.access
    }))
  ];
}

function hostMountPath(value: string): string {
  return stripVerbatimPrefix(value);
}

function containerPathForHost(mounts: OciMount[], hostPath: string): string | undefined {
  const match = mounts
    .filter(mount => pathInside(mount.host, hostPath))
    .sort((left, right) => comparablePath(right.host).length - comparablePath(left.host).length)[0];
  if (!match) return undefined;
  const relative = path.relative(stripVerbatimPrefix(match.host), stripVerbatimPrefix(hostPath)).replaceAll('\\', '/');
  return relative && relative !== '.' ? `${match.container}/${relative}` : match.container;
}

function windowsOnlyProgram(program: string): boolean {
  return /(?:\.exe|\.cmd|\.bat|\.ps1)$/i.test(path.basename(program));
}

function runtimeLabel(runtime: OciRuntime): string {
  return runtime === 'docker' ? 'Docker' : 'Podman';
}

function runCli(runtime: OciRuntime, cli: string, args: string[], timeoutMs = 30_000): Promise<CliResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(cli, args, { windowsHide: true, shell: false, stdio: ['ignore', 'pipe', 'pipe'] });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try { child.kill('SIGKILL'); } catch { /* best effort */ }
      reject(new OciSandboxError(runtime, 'SANDBOX_OCI_TIMEOUT', `${runtime} ${args[0] ?? ''} did not finish within ${timeoutMs} ms`, 'cli'));
    }, Math.max(1, timeoutMs));
    timer.unref();
    child.stdout.on('data', chunk => stdout.push(Buffer.from(chunk)));
    child.stderr.on('data', chunk => stderr.push(Buffer.from(chunk)));
    child.once('error', error => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new OciSandboxError(runtime, 'SANDBOX_OCI_UNAVAILABLE', `Failed to execute ${runtime} CLI: ${error.message}`, 'cli'));
    });
    child.once('close', code => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({ code, stdout: Buffer.concat(stdout).toString('utf8'), stderr: Buffer.concat(stderr).toString('utf8') });
    });
  });
}

function outputError(runtime: OciRuntime, code: string, message: string, stage: string, result: CliResult, suggestion?: string): OciSandboxError {
  return new OciSandboxError(runtime, code, message, stage, {
    exit_code: result.code,
    stdout: result.stdout,
    stderr: result.stderr,
    ...(suggestion ? { suggestion } : {})
  });
}

function containerMissing(result: CliResult): boolean {
  const message = `${result.stdout}\n${result.stderr}`.toLowerCase();
  return /no such container|no such object|not found/.test(message);
}

async function ensureEngineReady(runtime: OciRuntime, cli: string): Promise<void> {
  const result = await runCli(runtime, cli, ['info']);
  if (result.code === 0) return;
  throw outputError(
    runtime,
    'SANDBOX_OCI_UNAVAILABLE',
    `${runtimeLabel(runtime)} engine is not ready.`,
    'discovery',
    result,
    runtime === 'docker'
      ? 'Start Docker Desktop or the Docker daemon, then retry. The app does not start the engine for you.'
      : 'Start the Podman machine (`podman machine start`) or the Podman service, then retry. The app does not start it for you.'
  );
}

async function ensureImage(runtime: OciRuntime, cli: string, image: string): Promise<void> {
  const inspected = await runCli(runtime, cli, ['image', 'inspect', image]);
  if (inspected.code === 0) return;
  const pulled = await runCli(runtime, cli, ['pull', image], 300_000);
  if (pulled.code === 0) return;
  throw outputError(
    runtime,
    'SANDBOX_OCI_IMAGE_UNAVAILABLE',
    `${runtimeLabel(runtime)} could not inspect or pull the configured container image.`,
    'image',
    pulled,
    'Verify the image reference and registry/network access, then retry.'
  );
}

export async function prepareOci(
  runtime: OciRuntime,
  config: SandboxConfig,
  workspaceRoot: string
): Promise<OciPreparedSandbox> {
  const cli = discoverOciProgram(runtime);
  if (!cli) {
    throw new OciSandboxError(
      runtime,
      'SANDBOX_OCI_UNAVAILABLE',
      `${runtimeLabel(runtime)} CLI (${runtime}) was not found.`,
      'discovery'
    );
  }
  const image = selectedOciImage(runtime, config);
  const network = selectedOciNetwork(runtime, config);
  await ensureEngineReady(runtime, cli);
  const mounts = await buildOciMounts(runtime, workspaceRoot, config.externalPaths);
  await ensureImage(runtime, cli, image);
  return { runtime, cli, image, network, mounts };
}

export function buildOciRunArgs(
  prepared: OciPreparedSandbox,
  name: string,
  cwd: string,
  spec: ResolvedCommandSpec,
  environment: Array<[string, string]>,
  removeEnvironment: string[]
): string[] {
  const containerCwd = containerPathForHost(prepared.mounts, path.resolve(cwd));
  if (!containerCwd) {
    throw new OciSandboxError(
      prepared.runtime,
      'SANDBOX_OCI_PATH_UNMOUNTED',
      `Command working directory is not mounted in the ${runtimeLabel(prepared.runtime)} container: ${cwd}`,
      'launch'
    );
  }
  if (windowsOnlyProgram(spec.program)) {
    throw new OciSandboxError(
      prepared.runtime,
      'SANDBOX_OCI_COMMAND_UNSUPPORTED',
      `Windows host executable cannot run in a ${runtimeLabel(prepared.runtime)} Linux container: ${spec.program}`,
      'launch'
    );
  }
  let program = spec.program;
  if (path.isAbsolute(program)) {
    const mapped = containerPathForHost(prepared.mounts, path.resolve(program));
    if (!mapped) {
      throw new OciSandboxError(
        prepared.runtime,
        'SANDBOX_OCI_COMMAND_UNMOUNTED',
        `Sandbox command path is outside mounted workspaces: ${program}`,
        'launch'
      );
    }
    program = mapped;
  } else if (program.includes('/') || program.includes('\\')) {
    throw new OciSandboxError(
      prepared.runtime,
      'SANDBOX_OCI_COMMAND_UNAVAILABLE',
      `Relative sandbox command path was not resolved inside a mounted workspace: ${program}`,
      'launch'
    );
  }
  if (!program.trim()) {
    throw new OciSandboxError(prepared.runtime, 'SANDBOX_PROCESS_PLAN_INVALID', `${runtimeLabel(prepared.runtime)} process plan has an empty command.`, 'launch');
  }

  const effective = new Map(environment);
  const removed = new Set(removeEnvironment);
  for (const key of removed) effective.delete(key);

  const args = ['run', '--rm', '-i', '--name', name, '--network', prepared.network, '--security-opt', 'no-new-privileges'];
  for (const mount of prepared.mounts) {
    const suffix = mount.access === 'read_only' ? ':ro' : '';
    args.push('-v', `${hostMountPath(mount.host)}:${mount.container}${suffix}`);
  }
  args.push('-w', containerCwd);
  for (const [key, value] of [...effective.entries()].sort(([left], [right]) => left.localeCompare(right))) {
    args.push('-e', `${key}=${value}`);
  }
  args.push(prepared.image);
  if (removed.size) {
    args.push('env');
    for (const key of [...removed].sort()) args.push('-u', key);
  }
  args.push(program, ...spec.argv);
  return args;
}

async function cancelContainer(runtime: OciRuntime, cli: string, name: string): Promise<void> {
  let last: CliResult | undefined;
  for (let attempt = 0; attempt < 20; attempt += 1) {
    const result = await runCli(runtime, cli, ['rm', '-f', name], 10_000);
    if (result.code === 0 || containerMissing(result)) return;
    last = result;
    await new Promise(resolve => setTimeout(resolve, 50));
  }
  throw outputError(runtime, 'SANDBOX_OCI_CANCEL_FAILED', `Failed to remove ${runtimeLabel(runtime)} container ${name}.`, 'cancel', last ?? { code: null, stdout: '', stderr: '' });
}

export function prepareOciLaunch(
  prepared: OciPreparedSandbox,
  cwd: string,
  spec: ResolvedCommandSpec,
  environment: Array<[string, string]>,
  removeEnvironment: string[]
): SandboxLaunch {
  const name = `ctmcp-node-${prepared.runtime}-${randomUUID().replaceAll('-', '')}`;
  const args = buildOciRunArgs(prepared, name, cwd, spec, environment, removeEnvironment);
  let cleanupPromise: Promise<void> | undefined;
  return {
    program: prepared.cli,
    args,
    environmentMode: 'forwarded',
    kill: () => cancelContainer(prepared.runtime, prepared.cli, name),
    cleanup: () => {
      if (cleanupPromise) return cleanupPromise;
      cleanupPromise = cancelContainer(prepared.runtime, prepared.cli, name).catch(() => undefined);
      return cleanupPromise;
    },
    processTreeContained: true,
    processTreeControl: `${prepared.runtime}_container`
  };
}
