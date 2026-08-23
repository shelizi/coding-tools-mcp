import type { JsonObject, ToolContext } from '../types.js';
import { DEFAULT_COMMAND_TIMEOUT_MAX_MS } from '../executionLimits.js';
import { classifyCommandKind } from '../toolUsage.js';

const DEFAULT_PROCESS_TIMEOUT_MS = 30_000;

export function boundedInteger(value: unknown, fallback: number, minimum: number, maximum: number): number {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? Math.max(minimum, Math.min(maximum, Math.trunc(parsed))) : fallback;
}

export function commandTimeoutMaxMs(ctx: ToolContext): number {
  return ctx.config.limits.commandTimeoutMaxMs ?? DEFAULT_COMMAND_TIMEOUT_MAX_MS;
}

function packageScriptLooksLongRunning(commandDisplay: string): boolean {
  const value = commandDisplay.toLowerCase();
  const longScript = /(?:build|portable|package|release|verify|test|check|sync|parity)/;
  const packageManager = /(?:^|[\\/\s])(?:npm(?:\.cmd)?|pnpm(?:\.cmd)?|yarn(?:\.cmd)?|bun(?:\.exe)?)(?:\s|$)/.test(value);
  if (!packageManager) return false;
  if (value.includes(' run ')) return value.split(/\s+/).some(token => longScript.test(token));
  return /(?:^|[\\/\s])(?:pnpm(?:\.cmd)?|yarn(?:\.cmd)?|bun(?:\.exe)?)\s+([^\s]+)/.test(value)
    && longScript.test(value.match(/(?:^|[\\/\s])(?:pnpm(?:\.cmd)?|yarn(?:\.cmd)?|bun(?:\.exe)?)\s+([^\s]+)/)?.[1] ?? '');
}

export function resolvedCommandTimeoutMs(
  args: JsonObject,
  commandDisplay: string,
  timeoutMaxMs = DEFAULT_COMMAND_TIMEOUT_MAX_MS
): number {
  const maximum = Math.max(1, timeoutMaxMs);
  if (args.timeout_ms !== undefined) {
    return boundedInteger(args.timeout_ms, DEFAULT_PROCESS_TIMEOUT_MS, 1, maximum);
  }
  const commandKind = classifyCommandKind(args);
  const lowered = commandDisplay.toLowerCase();
  const longRunning = ['cargo_test', 'cargo_check', 'build'].includes(commandKind)
    || /(?:^|\s)cargo(?:\.exe)?\s+(?:build|check|test|clippy)(?:\s|$)/.test(lowered)
    || /(?:^|\s)tauri(?:\.exe)?\s+build(?:\s|$)/.test(lowered)
    || packageScriptLooksLongRunning(commandDisplay);
  return longRunning ? maximum : Math.min(DEFAULT_PROCESS_TIMEOUT_MS, maximum);
}
