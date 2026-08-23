import type { JsonObject } from '../types.js';
import {
  ProcessStartupError,
  startupDiagnosticsJson,
  WINDOWS_DLL_INIT_FAILED_SIGNED
} from '../processStartup.js';

export class ProcessToolError extends Error {
  readonly code: string;
  readonly category: string;
  readonly retryable: boolean;
  readonly details: JsonObject;

  constructor(code: string, message: string, category = 'runtime', retryable = false, details: JsonObject = {}) {
    super(message);
    this.name = 'ProcessToolError';
    this.code = code;
    this.category = category;
    this.retryable = retryable;
    this.details = details;
  }
}

export function startupToolError(error: unknown): ProcessToolError {
  if (!(error instanceof ProcessStartupError)) {
    return new ProcessToolError(
      'COMMAND_SPAWN_FAILED',
      `Failed to start command: ${error instanceof Error ? error.message : String(error)}`,
      'runtime',
      true,
      {
        termination_reason: 'spawn_failed',
        recoverable: true,
        suggestion: '检查命令路径、权限和运行时环境后重试'
      }
    );
  }
  const startup = startupDiagnosticsJson(error.diagnostics);
  if (error.kind === 'loader_initialization') {
    return new ProcessToolError(
      'COMMAND_START_TRANSIENT_FAILURE',
      'Windows could not initialize the child process after controlled retries.',
      'runtime',
      true,
      {
        message: error.message,
        termination_reason: 'loader_initialization_failed',
        recoverable: true,
        process_exit_code: error.exitCode ?? WINDOWS_DLL_INIT_FAILED_SIGNED,
        ntstatus: '0xc0000142',
        startup
      }
    );
  }
  if (error.kind === 'cancelled') {
    return new ProcessToolError(
      'COMMAND_START_CANCELLED',
      'Command startup was cancelled before a process session was retained.',
      'runtime',
      true,
      {
        termination_reason: 'request_cancelled',
        recoverable: true,
        startup
      }
    );
  }
  if (error.kind === 'timeout') {
    return new ProcessToolError(
      'COMMAND_START_TIMEOUT',
      'Command startup exhausted the configured timeout before a process session was retained.',
      'runtime',
      true,
      {
        termination_reason: 'process_timeout',
        recoverable: true,
        startup
      }
    );
  }
  return new ProcessToolError(
    'COMMAND_SPAWN_FAILED',
    `Failed to start command: ${error.message}`,
    'runtime',
    true,
    {
      termination_reason: 'spawn_failed',
      recoverable: true,
      suggestion: '检查命令路径、权限和运行时环境后重试',
      startup
    }
  );
}
