import { statSync } from 'node:fs';
import path from 'node:path';

export interface NativeLaunchSpec {
  program: string;
  args: string[];
  windowsVerbatimArguments?: boolean;
}

function environmentValue(environment: NodeJS.ProcessEnv, name: string): string | undefined {
  const direct = environment[name];
  if (direct !== undefined) return direct;
  return Object.entries(environment).find(([key]) => key.toLowerCase() === name.toLowerCase())?.[1];
}

function existingRegularFile(value: string): boolean {
  try {
    return statSync(value).isFile();
  } catch {
    return false;
  }
}

function resolveWindowsPathProgram(program: string, cwd: string, environment: NodeJS.ProcessEnv): string | undefined {
  if (path.isAbsolute(program) || program.includes('/') || program.includes('\\')) {
    return existingRegularFile(program) ? path.normalize(program) : undefined;
  }
  const searchPath = environmentValue(environment, 'PATH') ?? '';
  const pathExt = environmentValue(environment, 'PATHEXT') ?? '.COM;.EXE;.BAT;.CMD';
  const extensions = path.extname(program)
    ? ['']
    : pathExt.split(';').map(value => value.trim()).filter(Boolean);
  for (const directory of [cwd, ...searchPath.split(path.delimiter)]) {
    const base = directory.trim().replace(/^\"|\"$/g, '') || cwd;
    for (const extension of extensions) {
      const candidate = path.resolve(base, `${program}${extension}`);
      if (existingRegularFile(candidate)) return candidate;
    }
  }
  return undefined;
}

function windowsCommandPath(value: string): string {
  return value.startsWith('\\\\?\\') ? value.slice(4) : value;
}

function windowsBatchToken(value: string): string {
  return `\"${value.replaceAll('\"', '\"\"')}\"`;
}

function windowsBatchCommandLine(program: string, args: string[]): string {
  return ['call', windowsBatchToken(windowsCommandPath(program)), ...args.map(windowsBatchToken)].join(' ');
}

function powershellLiteral(value: string): string {
  return `'${value.replaceAll("'", "''")}'`;
}

export function nativeLaunchSpec(
  program: string,
  args: string[],
  cwd: string,
  environment: NodeJS.ProcessEnv,
  platform: NodeJS.Platform = process.platform
): NativeLaunchSpec {
  if (platform !== 'win32') return { program, args };
  const resolved = resolveWindowsPathProgram(program, cwd, environment) ?? program;
  const extension = path.extname(resolved).toLowerCase();
  if (extension === '.cmd' || extension === '.bat') {
    return {
      program: environmentValue(environment, 'COMSPEC') || 'cmd.exe',
      args: ['/d', '/s', '/c', windowsBatchCommandLine(resolved, args)],
      windowsVerbatimArguments: true
    };
  }
  if (extension === '.ps1') {
    const powershell = resolveWindowsPathProgram('pwsh.exe', cwd, environment)
      ?? resolveWindowsPathProgram('powershell.exe', cwd, environment)
      ?? 'powershell.exe';
    const invocation = ['&', powershellLiteral(windowsCommandPath(resolved)), ...args.map(powershellLiteral)].join(' ');
    return {
      program: powershell,
      args: ['-NoLogo', '-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-Command', invocation]
    };
  }
  return { program: resolved, args };
}
