import { spawn } from 'node:child_process';
import path from 'node:path';
import type { JsonObject } from './types.js';

export interface WslLocation {
  distro: string;
  linuxPath: string;
}

export interface WslInvocation {
  program: 'wsl.exe';
  args: string[];
}

export interface WslCommandResult {
  code: number | null;
  stdout: Buffer;
  stderr: Buffer;
}

export type WslRunner = (program: string, args: string[]) => Promise<WslCommandResult>;

export class WslRoutingError extends Error {
  constructor(
    readonly code: string,
    message: string,
    readonly category = 'validation',
    readonly retryable = false,
    readonly details: JsonObject = {}
  ) {
    super(message);
    this.name = 'WslRoutingError';
  }
}

export function normalizeLinuxPath(value: string): string {
  const segments = value.trim().replaceAll('\\', '/').split('/')
    .filter(segment => segment.length > 0 && segment !== '.');
  return segments.length ? `/${segments.join('/')}` : '/';
}

function validDistro(value: string): boolean {
  return Boolean(value)
    && !value.includes('/')
    && !value.includes('\\')
    && !value.includes('\0')
    && ![...value].some(character => /[\u0000-\u001f\u007f]/.test(character));
}

export function parseWslUncPath(value: string): WslLocation | undefined {
  let normalized = value.trim().replaceAll('/', '\\');
  const extendedPrefix = '\\\\?\\UNC\\';
  if (normalized.toLowerCase().startsWith(extendedPrefix.toLowerCase())) {
    normalized = `\\\\${normalized.slice(extendedPrefix.length)}`;
  }
  if (!normalized.startsWith('\\\\')) return undefined;
  const parts = normalized.slice(2).split('\\');
  const server = parts.shift()?.toLowerCase();
  if (server !== 'wsl.localhost' && server !== 'wsl$') return undefined;
  const distro = parts.shift()?.trim() ?? '';
  if (!validDistro(distro)) return undefined;
  return { distro, linuxPath: normalizeLinuxPath(parts.join('/')) };
}

export function wslUncPath(distroValue: string, linuxPathValue: string): string {
  const distro = distroValue.trim();
  if (!validDistro(distro)) throw new WslRoutingError('WSL_DISTRIBUTION_INVALID', 'WSL distribution name is invalid');
  const linuxPath = normalizeLinuxPath(linuxPathValue);
  if (linuxPath === '/') return `\\\\wsl.localhost\\${distro}`;
  return `\\\\wsl.localhost\\${distro}\\${linuxPath.slice(1).replaceAll('/', '\\')}`;
}

export function compareWslPaths(left: string, right: string): boolean | undefined {
  const leftLocation = parseWslUncPath(left);
  const rightLocation = parseWslUncPath(right);
  if (!leftLocation && !rightLocation) return undefined;
  if (!leftLocation || !rightLocation) return false;
  return leftLocation.distro.toLowerCase() === rightLocation.distro.toLowerCase()
    && leftLocation.linuxPath === rightLocation.linuxPath;
}

function normalizedWindowsIdentity(value: string): string | undefined {
  let normalized = value.trim().replaceAll('/', '\\');
  const extendedUnc = '\\\\?\\UNC\\';
  if (normalized.toLowerCase().startsWith(extendedUnc.toLowerCase())) {
    normalized = `\\\\${normalized.slice(extendedUnc.length)}`;
  } else if (normalized.toLowerCase().startsWith('\\\\?\\')) {
    normalized = normalized.slice(4);
  }
  if (!looksLikeWindowsDrivePath(normalized) && !normalized.startsWith('\\\\')) return undefined;
  normalized = path.win32.normalize(normalized);
  const root = path.win32.parse(normalized).root;
  while (normalized.length > root.length && normalized.endsWith('\\')) normalized = normalized.slice(0, -1);
  return normalized.toLowerCase();
}

export function workspacePathIdentity(value: string): string {
  const wsl = parseWslUncPath(value);
  if (wsl) return `wsl:${wsl.distro.toLowerCase()}:${wsl.linuxPath}`;
  const windows = normalizedWindowsIdentity(value);
  if (windows) return `windows:${windows}`;
  let native = path.resolve(value);
  const root = path.parse(native).root;
  while (native.length > root.length && (native.endsWith('/') || native.endsWith('\\'))) native = native.slice(0, -1);
  return `native:${native}`;
}

export function sameWorkspacePath(left: string, right: string): boolean {
  return workspacePathIdentity(left) === workspacePathIdentity(right);
}

export function normalizeWorkspacePath(value: string): string {
  const location = parseWslUncPath(value);
  return location ? wslUncPath(location.distro, location.linuxPath) : path.resolve(value);
}

export function workspaceBasename(value: string): string {
  const location = parseWslUncPath(value);
  return location ? path.posix.basename(location.linuxPath) || location.distro : path.basename(value);
}

export function looksLikeWindowsDrivePath(value: string): boolean {
  return /^[A-Za-z]:[\\/]/.test(value);
}

export function validateWslExecPaths(cwd: string, program: string, args: readonly string[]): void {
  const workspace = parseWslUncPath(cwd);
  if (!workspace) return;
  const values: Array<[string, string]> = [['program', program], ...args.map((value, index) => [`args[${index}]`, value] as [string, string])];
  for (const [position, value] of values) {
    const location = parseWslUncPath(value);
    if (location) {
      if (location.distro.toLowerCase() !== workspace.distro.toLowerCase()) {
        throw new WslRoutingError(
          'WSL_CROSS_DISTRIBUTION_PATH',
          `${position} references WSL distribution '${location.distro}' while the workspace runs in '${workspace.distro}'`,
          'validation',
          false,
          {
            position,
            path: value,
            workspace_distro: workspace.distro,
            path_distro: location.distro,
            suggestion: 'Copy the file into the workspace distribution or pass a Linux path available inside that distribution.'
          }
        );
      }
      continue;
    }
    if (looksLikeWindowsDrivePath(value)) {
      throw new WslRoutingError(
        'WSL_HOST_PATH_REQUIRES_TRANSLATION',
        `${position} uses a Windows host path that is not valid as a Linux command argument`,
        'validation',
        true,
        {
          position,
          path: value,
          workspace_distro: workspace.distro,
          suggestion: 'Use a workspace-relative path or the corresponding Linux mount path such as /mnt/c/...'
        }
      );
    }
  }
}

export function wslInvocationForPath(
  cwd: string,
  program: string,
  args: readonly string[],
  environment: readonly (readonly [string, string])[] = [],
  removeEnvironment: readonly string[] = [],
  platform: NodeJS.Platform = process.platform
): WslInvocation | undefined {
  const workspace = parseWslUncPath(cwd);
  if (!workspace || platform !== 'win32') return undefined;
  validateWslExecPaths(cwd, program, args);
  const programLocation = parseWslUncPath(program);
  const innerProgram = programLocation && programLocation.distro.toLowerCase() === workspace.distro.toLowerCase()
    ? programLocation.linuxPath
    : program.replaceAll('\\', '/');
  const innerArgs = args.map(argument => {
    const location = parseWslUncPath(argument);
    return location && location.distro.toLowerCase() === workspace.distro.toLowerCase()
      ? location.linuxPath
      : argument;
  });
  const wrapped = [
    '--distribution', workspace.distro,
    '--cd', workspace.linuxPath,
    '--exec'
  ];
  if (environment.length || removeEnvironment.length) {
    wrapped.push('env');
    for (const key of removeEnvironment) wrapped.push('-u', key);
    for (const [key, value] of environment) wrapped.push(`${key}=${value}`);
  }
  wrapped.push(innerProgram, ...innerArgs);
  return { program: 'wsl.exe', args: wrapped };
}

export function decodeWslOutput(value: Buffer | string): string {
  if (typeof value === 'string') return value.replace(/^\uFEFF/, '');
  const looksUtf16 = value.length >= 2
    && value.length % 2 === 0
    && [...value.subarray(1).filter((_, index) => index % 2 === 0)].filter(byte => byte === 0).length > value.length / 8;
  return (looksUtf16 ? value.toString('utf16le') : value.toString('utf8')).replace(/^\uFEFF/, '');
}

export const runWslCommand: WslRunner = (program, args) => new Promise((resolve, reject) => {
  const child = spawn(program, args, { windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  child.stdout.on('data', (chunk: Buffer) => stdout.push(chunk));
  child.stderr.on('data', (chunk: Buffer) => stderr.push(chunk));
  child.once('error', reject);
  child.once('close', code => resolve({ code, stdout: Buffer.concat(stdout), stderr: Buffer.concat(stderr) }));
});

export async function validateWslWorkspacePath(
  value: string,
  runner: WslRunner = runWslCommand,
  platform: NodeJS.Platform = process.platform
): Promise<void> {
  const location = parseWslUncPath(value);
  if (!location) return;
  if (location.linuxPath.split('/').includes('..')) {
    throw new WslRoutingError('WSL_WORKSPACE_PATH_INVALID', 'WSL workspace path may not contain parent-directory segments');
  }
  if (platform !== 'win32') {
    throw new WslRoutingError('WSL_UNSUPPORTED_PLATFORM', 'WSL workspaces are supported only on Windows clients');
  }
  let result: WslCommandResult;
  try {
    result = await runner('wsl.exe', [
      '--distribution', location.distro,
      '--cd', location.linuxPath,
      '--exec', 'test', '-d', '.'
    ]);
  } catch (error) {
    throw new WslRoutingError(
      'WSL_UNAVAILABLE',
      `Could not start WSL: ${error instanceof Error ? error.message : String(error)}`,
      'runtime',
      true,
      { distro: location.distro, linux_path: location.linuxPath }
    );
  }
  if (result.code !== 0) {
    const stderr = decodeWslOutput(result.stderr).trim();
    throw new WslRoutingError(
      'WSL_WORKSPACE_UNAVAILABLE',
      stderr || `WSL workspace does not exist or is inaccessible: ${location.distro}:${location.linuxPath}`,
      'workspace_routing',
      true,
      { distro: location.distro, linux_path: location.linuxPath, exit_code: result.code, stderr }
    );
  }
}
