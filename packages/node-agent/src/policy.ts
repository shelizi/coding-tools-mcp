import { access, realpath, stat } from 'node:fs/promises';
import path from 'node:path';
import type { AgentConfig, JsonObject, ToolContext } from './types.js';
import { relativeInside, resolveFromWorkspace, resolveInside, rootAndCwd } from './workspace.js';
import { parseWslUncPath, validateWslExecPaths, WslRoutingError } from './wsl.js';

const BASIC_READ_ONLY_COMMANDS = ['pwd', 'ls', 'dir', 'cat', 'head', 'tail', 'grep', 'find', 'which', 'echo'];
const DEFAULT_ALLOWED_COMMANDS = [
  'pytest', 'python', 'python3', 'npm', 'npx', 'node', 'pnpm', 'yarn', 'make', 'mvn', 'mvnw',
  'gradle', 'gradlew', 'cargo', 'go', 'ruff', 'mypy', 'eslint', 'tsc', 'msbuild', 'dotnet',
  'deno', 'bun', 'ruby', 'java', 'javac', 'cmake', 'clang', 'gcc', 'g++', 'git', 'cmd',
  'powershell', 'pwsh', 'sh'
];
const DEFAULT_SCRIPT_EXTENSIONS = ['.exe', '.bat', '.cmd', '.ps1'];
const BLOCKED_ENVIRONMENT_KEYS = new Set([
  'PATH', 'PATHEXT', 'COMSPEC', 'LD_PRELOAD', 'LD_LIBRARY_PATH',
  'DYLD_INSERT_LIBRARIES', 'DYLD_LIBRARY_PATH'
]);
const NETWORK_PATTERN = /(https?:\/\/|urllib\.request|requests\.|http\.client|\bcurl\b|\bwget\b|\bssh\b|\bscp\b|\bftp\b)/i;
const DANGEROUS_PATTERN = /(git\s+reset\s+--hard|git\s+clean\s+-[^\r\n]*f|git\s+checkout\s+--\s+\.|(^|\s)rm\s+(-[^\r\n]*r[^\r\n]*f|--recursive)|remove-item\s+[^\r\n]*-recurse|(^|\s)(rmdir|del)\s+\/s\b)/i;
const INTERPRETER_MUTATION_PATTERN = /(shutil\.(rmtree|move)|os\.(remove|unlink|rmdir)|pathlib\.[^\s;]+\.(unlink|rename)|write_text|write_bytes|fs\.(writefile|writefilesync|unlink|rm)|set-content|out-file|new-item|files?\.(write|delete)|open\([^)]*['"]w)/i;

export interface ResolvedCommandSpec {
  program: string;
  argv: string[];
  display: string;
  shell: false;
}

export class PolicyError extends Error {
  readonly code: string;
  constructor(message: string, code = 'POLICY_REJECTED') {
    super(message);
    this.name = 'PolicyError';
    this.code = code;
  }
}

export function defaultPolicy(): AgentConfig['policy'] {
  return {
    allowedCommands: [...new Set([...DEFAULT_ALLOWED_COMMANDS, ...BASIC_READ_ONLY_COMMANDS])],
    workspaceLocalEntries: true,
    workspaceScriptExtensions: [...DEFAULT_SCRIPT_EXTENSIONS],
    maxPatchBytes: 200_000
  };
}

export function mergeAllowedCommands(configured: unknown): string[] {
  const defaults = defaultPolicy().allowedCommands;
  const values = Array.isArray(configured)
    ? configured
    : typeof configured === 'string'
      ? configured.split(',')
      : [];
  return [...new Set([...defaults, ...values.map(String).map(value => value.trim().toLowerCase()).filter(Boolean)])];
}

export function normalizeScriptExtensions(configured: unknown): string[] {
  const values = Array.isArray(configured)
    ? configured
    : typeof configured === 'string'
      ? configured.split(',')
      : [];
  const normalized = values.map(String).map(value => value.trim().toLowerCase()).filter(Boolean)
    .map(value => value.startsWith('.') ? value : `.${value}`);
  return normalized.length ? [...new Set(normalized)] : [...DEFAULT_SCRIPT_EXTENSIONS];
}

export function splitShellWords(command: string): string[] {
  const parts: string[] = [];
  let current = '';
  let quote: 'single' | 'double' | undefined;
  let escaped = false;
  for (const character of command) {
    if (escaped) {
      current += character;
      escaped = false;
      continue;
    }
    if (character === '\\' && quote !== 'single') {
      escaped = true;
      continue;
    }
    if (quote === 'single') {
      if (character === "'") quote = undefined;
      else current += character;
      continue;
    }
    if (quote === 'double') {
      if (character === '"') quote = undefined;
      else current += character;
      continue;
    }
    if (character === "'") { quote = 'single'; continue; }
    if (character === '"') { quote = 'double'; continue; }
    if (/\s/.test(character)) {
      if (current) { parts.push(current); current = ''; }
      continue;
    }
    current += character;
  }
  if (escaped || quote) throw new PolicyError('Invalid command syntax', 'INVALID_ARGUMENT');
  if (current) parts.push(current);
  return parts;
}

function hasForbiddenShellSyntax(command: string): boolean {
  if (/[\r\n]/.test(command)) return true;
  let quote: 'single' | 'double' | undefined;
  let escaped = false;
  for (let index = 0; index < command.length; index += 1) {
    const character = command[index];
    if (escaped) { escaped = false; continue; }
    if (quote === 'single') {
      if (character === "'") quote = undefined;
      continue;
    }
    if (quote === 'double') {
      if (character === '\\') escaped = true;
      else if (character === '"') quote = undefined;
      continue;
    }
    if (character === '\\') { escaped = true; continue; }
    if (character === "'") { quote = 'single'; continue; }
    if (character === '"') { quote = 'double'; continue; }
    if (';&|><`'.includes(character)) return true;
    if (character === '$' && ['(', '{'].includes(command[index + 1] ?? '')) return true;
  }
  return false;
}

function relativePathAllowed(value: string): boolean {
  if (path.isAbsolute(value)) return false;
  return !value.replaceAll('\\', '/').split('/').includes('..');
}

function commandTargetsProtectedRepositoryAsset(command: string): boolean {
  const normalized = command.toLowerCase().replaceAll('\\', '/');
  if (!normalized.includes('.git') && !normalized.includes('.github')) return false;
  return ['rm ', 'remove-item', 'rmdir', 'del ', 'unlink', 'rmtree', 'write_text', 'writefile', 'rename', 'move', 'checkout', 'clean ']
    .some(value => normalized.includes(value));
}

function commandContainsExternalPath(command: string): boolean {
  const normalized = command.replaceAll('\\', '/');
  return normalized.includes('../') || /(^|["'\s])\/(?!\/)/.test(normalized) || /\b[A-Z]:\//i.test(normalized);
}

function environmentKey(value: string): void {
  if (!/^[A-Za-z_][A-Za-z0-9_]{0,127}$/.test(value)) throw new PolicyError(`Invalid environment key: ${value}`, 'INVALID_ARGUMENT');
}

function validateEnvironment(argumentsValue: JsonObject): void {
  if (argumentsValue.env !== undefined) {
    if (!argumentsValue.env || typeof argumentsValue.env !== 'object' || Array.isArray(argumentsValue.env)) {
      throw new PolicyError('env must be an object of string values', 'INVALID_ARGUMENT');
    }
    const entries = Object.entries(argumentsValue.env as JsonObject);
    if (entries.length > 64) throw new PolicyError('env contains too many entries', 'INVALID_ARGUMENT');
    for (const [key, raw] of entries) {
      environmentKey(key);
      if ([...BLOCKED_ENVIRONMENT_KEYS].some(blocked => blocked.toLowerCase() === key.toLowerCase())) {
        throw new PolicyError(`Environment variable is protected: ${key}`, 'ENVIRONMENT_VARIABLE_PROTECTED');
      }
      if (typeof raw !== 'string') throw new PolicyError('env values must be strings', 'INVALID_ARGUMENT');
      if (raw.length > 4096 || raw.includes('\0')) throw new PolicyError(`Invalid environment value for ${key}`, 'INVALID_ARGUMENT');
    }
  }
  if (argumentsValue.remove_env !== undefined) {
    if (!Array.isArray(argumentsValue.remove_env)) throw new PolicyError('remove_env must be an array', 'INVALID_ARGUMENT');
    if (argumentsValue.remove_env.length > 64) throw new PolicyError('remove_env contains too many entries', 'INVALID_ARGUMENT');
    for (const raw of argumentsValue.remove_env) {
      if (typeof raw !== 'string') throw new PolicyError('remove_env entries must be strings', 'INVALID_ARGUMENT');
      environmentKey(raw);
    }
  }
}

function commandParts(argumentsValue: JsonObject): { command: string; executable: string; shell: string; argv: string[] } {
  const cmd = typeof argumentsValue.cmd === 'string' ? argumentsValue.cmd : undefined;
  const script = typeof argumentsValue.script === 'string' ? argumentsValue.script : undefined;
  const program = typeof argumentsValue.program === 'string' ? argumentsValue.program : undefined;
  if ([cmd, script, program].filter(value => value !== undefined).length !== 1) {
    throw new PolicyError('exec_command requires exactly one of cmd, script, or program', 'INVALID_ARGUMENT');
  }
  const shell = String(argumentsValue.shell ?? 'none').toLowerCase();
  if (!['none', 'cmd', 'powershell', 'sh'].includes(shell)) throw new PolicyError('shell must be none, cmd, powershell, or sh', 'INVALID_ARGUMENT');
  if (program !== undefined && shell !== 'none') throw new PolicyError('program/args mode requires shell=none', 'INVALID_ARGUMENT');
  if (script !== undefined && shell === 'none') throw new PolicyError('script mode requires shell=powershell, cmd, or sh', 'INVALID_ARGUMENT');
  if (shell !== 'none' && argumentsValue.confirm !== true) {
    throw new PolicyError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION: explicit shell execution requires confirm=true', 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');
  }
  let argv: string[] = [];
  let executable: string;
  let command: string;
  if (program !== undefined) {
    if (!program.trim()) throw new PolicyError('exec_command requires a non-empty command', 'INVALID_ARGUMENT');
    if (argumentsValue.args !== undefined && !Array.isArray(argumentsValue.args)) throw new PolicyError('args must be an array', 'INVALID_ARGUMENT');
    argv = Array.isArray(argumentsValue.args) ? argumentsValue.args.map(value => {
      if (typeof value !== 'string') throw new PolicyError('args entries must be strings', 'INVALID_ARGUMENT');
      return value;
    }) : [];
    executable = program;
    command = [program, ...argv].join(' ');
  } else {
    command = (cmd ?? script ?? '');
    if (!command.trim()) throw new PolicyError('exec_command requires a non-empty command', 'INVALID_ARGUMENT');
    if (shell === 'none') {
      const parts = splitShellWords(command);
      if (!parts.length) throw new PolicyError('Empty command', 'INVALID_ARGUMENT');
      [executable, ...argv] = parts;
    } else {
      executable = shell;
    }
  }
  if (command.length > 64_000) throw new PolicyError('Command is too long', 'INVALID_ARGUMENT');
  if (String(argumentsValue.filesystem_scope ?? 'workspace') !== 'workspace') {
    throw new PolicyError('EXTERNAL_EXECUTION_NOT_ALLOWED: exec_command only allows Workspace execution', 'EXTERNAL_EXECUTION_NOT_ALLOWED');
  }
  for (const key of ['workdir', 'cwd']) {
    const value = argumentsValue[key];
    if (value !== undefined && (typeof value !== 'string' || !relativePathAllowed(value))) {
      throw new PolicyError('workdir must stay inside the configured workspace', 'PATH_OUTSIDE_WORKSPACE');
    }
  }
  if (shell === 'none' && cmd !== undefined && hasForbiddenShellSyntax(cmd)) {
    throw new PolicyError('Shell chaining, redirection and expansion require an explicit shell mode', 'SHELL_MODE_REQUIRED');
  }
  return { command, executable, shell, argv };
}

function executableStem(executable: string): string {
  const base = executable.trim().replace(/^\.\//, '').split(/[\\/]/).at(-1)?.toLowerCase() ?? '';
  return base.replace(/\.(?:exe|cmd|bat)$/i, '');
}

async function insideWorkspace(candidate: string, root: string): Promise<boolean> {
  try {
    const [resolved, resolvedRoot, info] = await Promise.all([realpath(candidate), realpath(root), stat(candidate)]);
    if (!info.isFile()) return false;
    relativeInside(resolvedRoot, resolved);
    return true;
  } catch {
    return false;
  }
}

async function workspaceCandidate(ctx: ToolContext, key: string, argumentsValue: JsonObject, executable: string): Promise<boolean> {
  if (!ctx.config.policy.workspaceLocalEntries) return false;
  const { root, cwd: defaultCwd } = rootAndCwd(ctx, key);
  const cwd = resolveInside(root, String(argumentsValue.workdir ?? argumentsValue.cwd ?? relativeInside(root, defaultCwd)));
  let candidate: string;
  try {
    candidate = parseWslUncPath(executable)
      ? executable
      : resolveFromWorkspace(root, cwd, executable);
  } catch (error) {
    if (error instanceof WslRoutingError) throw error;
    return false;
  }
  if (await insideWorkspace(candidate, root)) return true;
  const base = path.basename(executable).toLowerCase();
  return (executable.includes('/') || executable.includes('\\'))
    && ctx.config.policy.workspaceScriptExtensions.some(extension => base.endsWith(extension));
}

async function validateCommand(ctx: ToolContext, key: string, argumentsValue: JsonObject): Promise<void> {
  const parts = commandParts(argumentsValue);
  const command = parts.command;
  const { root, cwd: defaultCwd } = rootAndCwd(ctx, key);
  const cwd = resolveInside(root, String(argumentsValue.workdir ?? argumentsValue.cwd ?? relativeInside(root, defaultCwd)));
  if (parseWslUncPath(root)) {
    if (parts.shell === 'cmd') throw new PolicyError('shell=cmd is unavailable for WSL workspaces; use shell=sh', 'INVALID_ARGUMENT');
    if (parts.shell === 'powershell') throw new PolicyError('shell=powershell is unavailable for WSL workspaces; use shell=sh', 'INVALID_ARGUMENT');
    validateWslExecPaths(cwd, parts.executable, parts.argv);
  }
  if ((DANGEROUS_PATTERN.test(command) || INTERPRETER_MUTATION_PATTERN.test(command)) && commandTargetsProtectedRepositoryAsset(command)) {
    throw new PolicyError('PROTECTED_REPOSITORY_ASSET: deleting or recursively clearing .git/.github is forbidden', 'PROTECTED_REPOSITORY_ASSET');
  }
  if (INTERPRETER_MUTATION_PATTERN.test(command) && commandContainsExternalPath(command)) {
    throw new PolicyError('WORKSPACE_PATH_PROTECTED: subprocess writes outside the Workspace are forbidden', 'WORKSPACE_PATH_PROTECTED');
  }
  if (DANGEROUS_PATTERN.test(command) && argumentsValue.confirm !== true) {
    throw new PolicyError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION: dangerous command requires confirm=true', 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');
  }
  if (!['trusted', 'dangerous'].includes(ctx.config.permissionMode) && NETWORK_PATTERN.test(command)) {
    throw new PolicyError('Network-looking commands are blocked in safe permission mode', 'NETWORK_COMMAND_BLOCKED');
  }
  const stem = executableStem(parts.executable);
  const allowed = ctx.config.policy.allowedCommands.includes(stem)
    || await workspaceCandidate(ctx, key, argumentsValue, parts.executable);
  if (!allowed) throw new PolicyError(`Command is not allowlisted: ${stem}`, 'COMMAND_REJECTED');
  validateEnvironment(argumentsValue);
  if (Number(argumentsValue.timeout_ms ?? 0) > 600_000) throw new PolicyError('Command timeout exceeds 10 minutes', 'INVALID_ARGUMENT');
  if (argumentsValue.post_checks !== undefined) {
    if (!Array.isArray(argumentsValue.post_checks)) throw new PolicyError('post_checks must be an array', 'INVALID_ARGUMENT');
    if (argumentsValue.post_checks.length > 16) throw new PolicyError('post_checks supports at most 16 checks', 'INVALID_ARGUMENT');
    for (let index = 0; index < argumentsValue.post_checks.length; index += 1) {
      const value = argumentsValue.post_checks[index];
      if (!value || typeof value !== 'object' || Array.isArray(value)) throw new PolicyError(`post_checks[${index}] must be an object`, 'INVALID_ARGUMENT');
      if ('post_checks' in value) throw new PolicyError('nested post_checks are not allowed', 'INVALID_ARGUMENT');
      try { await validateCommand(ctx, key, value as JsonObject); }
      catch (error) { throw new PolicyError(`post_checks[${index}] rejected: ${error instanceof Error ? error.message : String(error)}`, error instanceof PolicyError || error instanceof WslRoutingError ? error.code : 'POLICY_REJECTED'); }
    }
  }
}

function serializedBytes(value: unknown): number {
  return Buffer.byteLength(JSON.stringify(value));
}

function validateFormatFiles(argumentsValue: JsonObject, maxPatchBytes: number): void {
  if (serializedBytes(argumentsValue) > maxPatchBytes * 4) throw new PolicyError('format_files payload is too large', 'PAYLOAD_TOO_LARGE');
  const mode = String(argumentsValue.mode ?? 'plan');
  const scope = String(argumentsValue.scope ?? 'files');
  if (!['plan', 'check', 'apply'].includes(mode)) throw new PolicyError('format_files mode must be plan, check, or apply', 'INVALID_ARGUMENT');
  if (!['files', 'changed', 'staged', 'project'].includes(scope)) throw new PolicyError('format_files scope must be files, changed, staged, or project', 'INVALID_ARGUMENT');
  const pathValues: unknown[] = [];
  if (argumentsValue.paths !== undefined) {
    if (!Array.isArray(argumentsValue.paths)) throw new PolicyError('format_files paths must be an array', 'INVALID_ARGUMENT');
    pathValues.push(...argumentsValue.paths);
  }
  if (argumentsValue.expected_sha256 !== undefined) {
    if (!argumentsValue.expected_sha256 || typeof argumentsValue.expected_sha256 !== 'object' || Array.isArray(argumentsValue.expected_sha256)) {
      throw new PolicyError('format_files expected_sha256 must be an object', 'INVALID_ARGUMENT');
    }
    pathValues.push(...Object.keys(argumentsValue.expected_sha256 as JsonObject));
  }
  for (const value of pathValues) {
    if (typeof value !== 'string' || !value.trim() || !relativePathAllowed(value)) {
      throw new PolicyError('format_files paths must stay inside the configured workspace', 'PATH_OUTSIDE_WORKSPACE');
    }
  }
  for (const key of ['include_patterns', 'exclude_patterns']) {
    const values = argumentsValue[key];
    if (values === undefined) continue;
    if (!Array.isArray(values)) throw new PolicyError(`format_files ${key} must be an array`, 'INVALID_ARGUMENT');
    for (const value of values) {
      if (typeof value !== 'string' || !relativePathAllowed(value)) throw new PolicyError(`format_files ${key} must stay inside the configured workspace`, 'PATH_OUTSIDE_WORKSPACE');
    }
  }
  if (mode === 'apply' && scope === 'project' && argumentsValue.confirm !== true) {
    throw new PolicyError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION: project-wide formatting requires confirm=true', 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');
  }
  if (mode === 'apply' && Number(argumentsValue.max_files ?? 500) > 2_000 && argumentsValue.confirm !== true) {
    throw new PolicyError('DANGEROUS_OPERATION_REQUIRES_CONFIRMATION: formatting more than 2000 files requires confirm=true', 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION');
  }
}

export async function validateToolPolicy(ctx: ToolContext, key: string, toolName: string, argumentsValue: JsonObject): Promise<void> {
  const max = ctx.config.policy.maxPatchBytes;
  switch (toolName) {
    case 'exec_command':
      await validateCommand(ctx, key, argumentsValue);
      return;
    case 'exec_many': {
      if (!Array.isArray(argumentsValue.commands) || !argumentsValue.commands.length) throw new PolicyError('commands are required', 'INVALID_ARGUMENT');
      if (argumentsValue.commands.length > 256) throw new PolicyError('exec_many supports at most 256 commands', 'INVALID_ARGUMENT');
      for (let index = 0; index < argumentsValue.commands.length; index += 1) {
        const command = argumentsValue.commands[index];
        if (!command || typeof command !== 'object' || Array.isArray(command)) throw new PolicyError(`commands[${index}] must be an object`, 'INVALID_ARGUMENT');
        try { await validateCommand(ctx, key, command as JsonObject); }
        catch (error) { throw new PolicyError(`commands[${index}] rejected: ${error instanceof Error ? error.message : String(error)}`, error instanceof PolicyError || error instanceof WslRoutingError ? error.code : 'POLICY_REJECTED'); }
      }
      return;
    }
    case 'apply_patch':
    case 'patch_check': {
      if (typeof argumentsValue.patch !== 'string' || !argumentsValue.patch.trim()) throw new PolicyError('apply_patch requires a patch', 'INVALID_ARGUMENT');
      if (Buffer.byteLength(argumentsValue.patch) > max) throw new PolicyError('Patch is too large', 'PAYLOAD_TOO_LARGE');
      return;
    }
    case 'edit': {
      const files = Array.isArray(argumentsValue.files) ? argumentsValue.files : [];
      if (!files.length || files.length > 100) throw new PolicyError('edit requires between 1 and 100 files', 'INVALID_ARGUMENT');
      for (let index = 0; index < files.length; index += 1) {
        const file = files[index];
        if (!file || typeof file !== 'object' || Array.isArray(file)) throw new PolicyError(`edit files[${index}] must be an object`, 'INVALID_ARGUMENT');
        const item = file as JsonObject;
        if (typeof item.path !== 'string' || !item.path.trim()) throw new PolicyError(`edit files[${index}].path is required`, 'INVALID_ARGUMENT');
        if (Array.isArray(item.edits) && item.edits.length > 100) {
          throw new PolicyError(`edit files[${index}].edits supports at most 100 operations`, 'INVALID_ARGUMENT');
        }
      }
      if (serializedBytes(argumentsValue) > max * 4) throw new PolicyError('edit payload is too large', 'PAYLOAD_TOO_LARGE');
      return;
    }
    case 'edit_file': {
      if (typeof argumentsValue.path !== 'string' || !argumentsValue.path.trim()) throw new PolicyError('edit_file requires a non-empty path', 'INVALID_ARGUMENT');
      const edits = Array.isArray(argumentsValue.edits) && argumentsValue.edits.length > 0;
      const proposal = Boolean(argumentsValue.apply_proposal && typeof argumentsValue.apply_proposal === 'object' && !Array.isArray(argumentsValue.apply_proposal));
      if (!edits && !proposal) throw new PolicyError('edit_file requires non-empty edits or apply_proposal', 'INVALID_ARGUMENT');
      if (serializedBytes(argumentsValue) > max) throw new PolicyError('Edit payload is too large', 'PAYLOAD_TOO_LARGE');
      return;
    }
    case 'edit_many':
    case 'file_ops':
      if (serializedBytes(argumentsValue) > max * 4) throw new PolicyError(`${toolName} payload is too large`, 'PAYLOAD_TOO_LARGE');
      return;
    case 'format_files':
      validateFormatFiles(argumentsValue, max);
      return;
    default:
      return;
  }
}

function shellCommand(shell: string, script: string, wslWorkspace: boolean): ResolvedCommandSpec {
  if (wslWorkspace) {
    if (shell !== 'sh') throw new PolicyError(`shell=${shell} is unavailable for WSL workspaces; use shell=sh`, 'INVALID_ARGUMENT');
    return { program: 'sh', argv: ['-c', script], display: script, shell: false };
  }
  if (process.platform === 'win32') {
    if (shell === 'cmd') return { program: 'cmd.exe', argv: ['/d', '/s', '/c', script], display: script, shell: false };
    if (shell === 'sh') return { program: 'sh.exe', argv: ['-lc', script], display: script, shell: false };
    return { program: 'powershell.exe', argv: ['-NoProfile', '-NonInteractive', '-Command', script], display: script, shell: false };
  }
  if (shell === 'powershell') return { program: 'pwsh', argv: ['-NoProfile', '-NonInteractive', '-Command', script], display: script, shell: false };
  if (shell === 'cmd') throw new PolicyError('cmd shell is unavailable on this platform', 'COMMAND_REJECTED');
  return { program: '/bin/sh', argv: ['-lc', script], display: script, shell: false };
}

export async function resolveCommandSpec(ctx: ToolContext, key: string, argumentsValue: JsonObject): Promise<ResolvedCommandSpec> {
  const parts = commandParts(argumentsValue);
  const { root, cwd: defaultCwd } = rootAndCwd(ctx, key);
  const cwd = resolveInside(root, String(argumentsValue.workdir ?? argumentsValue.cwd ?? relativeInside(root, defaultCwd)));
  const wslWorkspace = Boolean(parseWslUncPath(root));
  if (parts.shell !== 'none') {
    const spec = shellCommand(parts.shell, parts.command, wslWorkspace);
    validateWslExecPaths(cwd, spec.program, spec.argv);
    return spec;
  }
  const raw = parts.executable.trim();
  validateWslExecPaths(cwd, raw, parts.argv);
  if (wslWorkspace && raw.startsWith('/') && ctx.config.policy.allowedCommands.includes(executableStem(raw))) {
    return { program: raw, argv: parts.argv, display: [raw, ...parts.argv].join(' '), shell: false };
  }
  const explicit = path.isAbsolute(raw) || raw.includes('/') || raw.includes('\\');
  if (!explicit) return { program: raw, argv: parts.argv, display: [raw, ...parts.argv].join(' '), shell: false };
  const candidate = resolveFromWorkspace(root, cwd, raw);
  let resolved: string;
  try {
    await access(candidate);
    resolved = await realpath(candidate);
  } catch {
    throw new PolicyError(`Program not found: ${raw}`, 'COMMAND_REJECTED');
  }
  if (!await insideWorkspace(resolved, root)) throw new PolicyError(`Workspace external executable rejected: ${raw}`, 'EXECUTABLE_OUTSIDE_WORKSPACE');
  const extension = (wslWorkspace ? path.posix.extname(raw.replaceAll('\\', '/')) : path.extname(resolved)).toLowerCase();
  if (!ctx.config.policy.workspaceLocalEntries || (extension && !ctx.config.policy.workspaceScriptExtensions.includes(extension))) {
    throw new PolicyError(`Workspace local entry is not allowed: ${raw}`, 'COMMAND_REJECTED');
  }
  return { program: resolved, argv: parts.argv, display: [raw, ...parts.argv].join(' '), shell: false };
}
