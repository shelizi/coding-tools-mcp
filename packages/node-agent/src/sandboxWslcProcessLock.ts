import { spawn, type ChildProcess } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync } from 'node:fs';
import path from 'node:path';

export const WSLC_STORAGE_ADMISSION_MAX_MS = 120_000;

const MUTEX_SCRIPT = String.raw`
$ErrorActionPreference = 'Stop'
$name = [string]$env:CTMCP_WSLC_LOCK_NAME
$timeoutMs = [int]$env:CTMCP_WSLC_LOCK_TIMEOUT_MS
$parentPid = [int]$env:CTMCP_WSLC_LOCK_PARENT_PID
$mutex = [System.Threading.Mutex]::new($false, $name)
$acquired = $false
try {
  try {
    $acquired = $mutex.WaitOne($timeoutMs)
  } catch [System.Threading.AbandonedMutexException] {
    $acquired = $true
  }
  if (-not $acquired) {
    [Console]::Error.WriteLine('timeout')
    exit 3
  }
  [Console]::Out.WriteLine('acquired')
  [Console]::Out.Flush()
  try {
    $parent = [System.Diagnostics.Process]::GetProcessById($parentPid)
    $parent.WaitForExit()
  } catch {
    # Parent already exited between acquisition and handle lookup.
  }
} finally {
  if ($acquired) {
    try { $mutex.ReleaseMutex() } catch {}
  }
  $mutex.Dispose()
}
`;

export class WslcStorageLockError extends Error {
  readonly category = 'runtime';
  readonly retryable: boolean;
  readonly details: Record<string, unknown>;

  constructor(code: string, message: string, retryable: boolean, details: Record<string, unknown> = {}) {
    super(message);
    this.name = 'WslcStorageLockError';
    this.code = code;
    this.retryable = retryable;
    this.details = {
      sandbox_backend: 'wslc',
      stage: 'storage_process_lock',
      fallback_allowed: false,
      ...details
    };
  }

  readonly code: string;
}

export interface WslcStorageProcessLock {
  waitMs: number;
  release(): Promise<void>;
}

export function windowsPowerShellPath(
  windowsDirectory = process.env.WINDIR ?? process.env.SystemRoot ?? 'C:\\Windows'
): string {
  return path.join(windowsDirectory, 'System32', 'WindowsPowerShell', 'v1.0', 'powershell.exe');
}

export function wslcStorageProcessLockHostAvailable(): boolean {
  return process.platform === 'win32' && existsSync(windowsPowerShellPath());
}

export function wslcStorageMutexName(storage: string): string {
  let identity = path.resolve(storage).replaceAll('\\', '/').replace(/\/+$/, '');
  if (process.platform === 'win32') identity = identity.toLowerCase();
  const digest = createHash('sha256').update(identity).digest('hex');
  return `Local\\CodingToolsMCP.WslcStorage.${digest}`;
}

function terminateHolder(child: ChildProcess): void {
  if (child.exitCode === null && child.signalCode === null) {
    try { child.kill('SIGKILL'); } catch { /* best effort */ }
  }
}

export async function acquireWslcStorageProcessLock(
  storage: string,
  signal?: AbortSignal,
  timeoutMs = WSLC_STORAGE_ADMISSION_MAX_MS
): Promise<WslcStorageProcessLock> {
  if (signal?.aborted) {
    throw new WslcStorageLockError(
      'SANDBOX_WSLC_QUEUE_CANCELLED',
      'WSLC storage admission was cancelled before the cross-process lock could be acquired.',
      true
    );
  }
  const boundedTimeoutMs = Math.max(1, Math.min(WSLC_STORAGE_ADMISSION_MAX_MS, Math.trunc(timeoutMs)));
  const powershell = windowsPowerShellPath();
  if (!existsSync(powershell)) {
    throw new WslcStorageLockError(
      'SANDBOX_WSLC_PROCESS_LOCK_UNAVAILABLE',
      `Windows PowerShell is required for the WSLC cross-process storage lock: ${powershell}`,
      false
    );
  }

  const started = Date.now();
  const child = spawn(powershell, [
    '-NoLogo',
    '-NoProfile',
    '-NonInteractive',
    '-ExecutionPolicy',
    'Bypass',
    '-Command',
    MUTEX_SCRIPT
  ], {
    windowsHide: true,
    shell: false,
    stdio: ['ignore', 'pipe', 'pipe'],
    env: {
      ...process.env,
      CTMCP_WSLC_LOCK_NAME: wslcStorageMutexName(storage),
      CTMCP_WSLC_LOCK_TIMEOUT_MS: String(boundedTimeoutMs),
      CTMCP_WSLC_LOCK_PARENT_PID: String(process.pid)
    }
  });

  let stdout = '';
  let stderr = '';
  let settled = false;
  let onAbort: (() => void) | undefined;
  const acquired = await new Promise<boolean>((resolve, reject) => {
    const finish = (value: boolean) => {
      if (settled) return;
      settled = true;
      if (signal && onAbort) signal.removeEventListener('abort', onAbort);
      resolve(value);
    };
    child.stdout.on('data', chunk => {
      stdout += Buffer.from(chunk).toString('utf8');
      if (stdout.split(/\r?\n/).some(line => line.trim() === 'acquired')) finish(true);
    });
    child.stderr.on('data', chunk => { stderr += Buffer.from(chunk).toString('utf8'); });
    child.once('error', error => {
      if (settled) return;
      settled = true;
      if (signal && onAbort) signal.removeEventListener('abort', onAbort);
      reject(new WslcStorageLockError(
        'SANDBOX_WSLC_PROCESS_LOCK_FAILED',
        `Failed to start the WSLC cross-process storage lock holder: ${error.message}`,
        true
      ));
    });
    child.once('exit', code => {
      if (settled) return;
      if (code === 3 || /timeout/i.test(stderr)) {
        finish(false);
        return;
      }
      settled = true;
      if (signal && onAbort) signal.removeEventListener('abort', onAbort);
      reject(new WslcStorageLockError(
        'SANDBOX_WSLC_PROCESS_LOCK_FAILED',
        `WSLC cross-process storage lock holder exited before acquisition (exit ${code ?? 'unknown'}).`,
        true,
        { stderr: stderr.trim() }
      ));
    });
    if (signal) {
      onAbort = () => {
        if (settled) return;
        settled = true;
        terminateHolder(child);
        reject(new WslcStorageLockError(
          'SANDBOX_WSLC_QUEUE_CANCELLED',
          'WSLC storage admission was cancelled while waiting for the cross-process lock.',
          true
        ));
      };
      signal.addEventListener('abort', onAbort, { once: true });
    }
  });

  if (!acquired) {
    terminateHolder(child);
    throw new WslcStorageLockError(
      'SANDBOX_WSLC_PROCESS_LOCK_TIMEOUT',
      `Timed out after ${boundedTimeoutMs} ms waiting for another Node Agent to release WSLC session storage.`,
      true,
      { storage_lock_wait_ms: Date.now() - started, timeout_ms: boundedTimeoutMs }
    );
  }

  let releasePromise: Promise<void> | undefined;
  return {
    waitMs: Date.now() - started,
    release() {
      if (releasePromise) return releasePromise;
      releasePromise = new Promise<void>(resolve => {
        if (child.exitCode !== null || child.signalCode !== null) {
          resolve();
          return;
        }
        const timer = setTimeout(() => {
          terminateHolder(child);
          resolve();
        }, 2_000);
        timer.unref();
        child.once('exit', () => {
          clearTimeout(timer);
          resolve();
        });
        terminateHolder(child);
      });
      return releasePromise;
    }
  };
}
