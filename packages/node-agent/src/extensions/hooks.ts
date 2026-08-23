import { spawn } from 'node:child_process';
import type { JsonObject } from '../types.js';
import type { HookDescriptor, HookPostResult, HookPreResult } from './types.js';

const MAX_HOOK_OUTPUT_BYTES = 256 * 1024;

function matches(matcher: string | undefined, toolName: string): boolean {
  if (!matcher?.trim()) return true;
  const value = matcher.trim();
  if (value.includes('|') && !/[()[\]{}+*?^$\\]/.test(value)) {
    return value.split('|').some(item => item.trim() === toolName);
  }
  if (value === toolName) return true;
  try {
    return new RegExp(value).test(toolName);
  } catch {
    return false;
  }
}

function jsonRecord(value: unknown): JsonObject | undefined {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : undefined;
}

async function runCommandHook(
  hook: HookDescriptor,
  input: JsonObject,
  cwd: string
): Promise<{ code: number | null; stdout: string; stderr: string; timedOut: boolean }> {
  const command = hook.command?.trim();
  if (!command) return { code: 0, stdout: '', stderr: '', timedOut: false };
  const args = hook.args ?? [];
  const child = args.length
    ? spawn(command, args, { cwd, env: process.env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true })
    : spawn(command, { cwd, env: process.env, shell: true, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
  const stdout: Buffer[] = [];
  const stderr: Buffer[] = [];
  let stdoutBytes = 0;
  let stderrBytes = 0;
  child.stdout.on('data', chunk => {
    const buffer = Buffer.from(chunk);
    if (stdoutBytes < MAX_HOOK_OUTPUT_BYTES) stdout.push(buffer.subarray(0, MAX_HOOK_OUTPUT_BYTES - stdoutBytes));
    stdoutBytes += buffer.length;
  });
  child.stderr.on('data', chunk => {
    const buffer = Buffer.from(chunk);
    if (stderrBytes < MAX_HOOK_OUTPUT_BYTES) stderr.push(buffer.subarray(0, MAX_HOOK_OUTPUT_BYTES - stderrBytes));
    stderrBytes += buffer.length;
  });
  child.stdin.end(`${JSON.stringify(input)}\n`);
  const timeoutMs = Math.max(100, Math.min(hook.timeoutMs || 10_000, 120_000));
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    child.kill();
  }, timeoutMs);
  const code = await new Promise<number | null>((resolve, reject) => {
    child.once('error', reject);
    child.once('exit', resolve);
  }).finally(() => clearTimeout(timer));
  return {
    code,
    stdout: Buffer.concat(stdout).toString('utf8').trim(),
    stderr: Buffer.concat(stderr).toString('utf8').trim(),
    timedOut
  };
}

async function runHttpHook(hook: HookDescriptor, input: JsonObject): Promise<{ code: number; stdout: string; stderr: string; timedOut: boolean }> {
  if (!hook.url) return { code: 0, stdout: '', stderr: '', timedOut: false };
  const controller = new AbortController();
  const timeoutMs = Math.max(100, Math.min(hook.timeoutMs || 10_000, 120_000));
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    const response = await fetch(hook.url, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(input),
      signal: controller.signal
    });
    return { code: response.ok ? 0 : response.status, stdout: (await response.text()).slice(0, MAX_HOOK_OUTPUT_BYTES), stderr: '', timedOut: false };
  } catch (error) {
    return { code: 1, stdout: '', stderr: error instanceof Error ? error.message : String(error), timedOut: controller.signal.aborted };
  } finally {
    clearTimeout(timer);
  }
}

async function runHook(hook: HookDescriptor, input: JsonObject, cwd: string) {
  if (hook.handlerType === 'command') return runCommandHook(hook, input, cwd);
  if (hook.handlerType === 'http') return runHttpHook(hook, input);
  return { code: 0, stdout: '', stderr: '', timedOut: false };
}

function parseHookOutput(text: string): JsonObject | undefined {
  if (!text.trim()) return undefined;
  try {
    return jsonRecord(JSON.parse(text));
  } catch {
    return undefined;
  }
}

function hookSpecific(output: JsonObject | undefined): JsonObject | undefined {
  return jsonRecord(output?.hookSpecificOutput);
}

function blockReason(output: JsonObject | undefined, stderr: string): string | undefined {
  const specific = hookSpecific(output);
  const decision = String(specific?.permissionDecision ?? output?.decision ?? '').toLowerCase();
  if (decision === 'deny' || decision === 'block') {
    return String(specific?.permissionDecisionReason ?? output?.reason ?? output?.message ?? stderr ?? 'Blocked by Hook');
  }
  if (output?.continue === false) return String(output.stopReason ?? output.reason ?? 'Blocked by Hook');
  return undefined;
}

export async function runSessionHooks(
  hooks: readonly HookDescriptor[],
  event: 'SessionStart' | 'SessionEnd',
  cwd: string,
  sessionId: string,
  source: string
): Promise<HookPostResult> {
  const feedback: string[] = [];
  for (const hook of hooks) {
    if (!matches(hook.matcher, source)) continue;
    const payload: JsonObject = {
      session_id: sessionId,
      cwd,
      hook_event_name: event,
      source
    };
    try {
      const result = await runHook(hook, payload, cwd);
      const output = parseHookOutput(result.stdout);
      const specific = hookSpecific(output);
      if (result.timedOut) {
        feedback.push('Hook timed out.');
        continue;
      }
      const additional = String(specific?.additionalContext ?? output?.additionalContext ?? output?.message ?? '').trim();
      if (additional) feedback.push(additional);
      else if (result.code !== 0 && result.stderr) feedback.push(result.stderr);
    } catch (error) {
      feedback.push(`Hook execution failed: ${error instanceof Error ? error.message : String(error)}`);
    }
  }
  return { feedback };
}

export async function runPreToolHooks(
  hooks: readonly HookDescriptor[],
  toolName: string,
  input: JsonObject,
  cwd: string,
  sessionId: string
): Promise<HookPreResult> {
  let current = structuredClone(input);
  const context: string[] = [];
  for (const hook of hooks) {
    if (!matches(hook.matcher, toolName)) continue;
    const payload: JsonObject = {
      session_id: sessionId,
      cwd,
      hook_event_name: 'PreToolUse',
      tool_name: toolName,
      tool_input: current
    };
    const result = await runHook(hook, payload, cwd);
    const output = parseHookOutput(result.stdout);
    if (result.timedOut) return { input: current, blocked: { message: 'Hook timed out.', hookKey: hook.key }, context };
    const reason = blockReason(output, result.stderr);
    if (result.code === 2 || reason) {
      return { input: current, blocked: { message: reason ?? (result.stderr || 'Blocked by Hook.'), hookKey: hook.key }, context };
    }
    const specific = hookSpecific(output);
    const updated = jsonRecord(specific?.updatedInput ?? output?.updatedInput);
    if (updated) current = structuredClone(updated);
    const additional = String(specific?.additionalContext ?? output?.additionalContext ?? '').trim();
    if (additional) context.push(additional);
  }
  return { input: current, context };
}

export async function runPostToolHooks(
  hooks: readonly HookDescriptor[],
  event: 'PostToolUse' | 'PostToolUseFailure',
  toolName: string,
  input: JsonObject,
  response: JsonObject,
  cwd: string,
  sessionId: string
): Promise<HookPostResult> {
  const feedback: string[] = [];
  for (const hook of hooks) {
    if (!matches(hook.matcher, toolName)) continue;
    const payload: JsonObject = {
      session_id: sessionId,
      cwd,
      hook_event_name: event,
      tool_name: toolName,
      tool_input: input,
      tool_response: response
    };
    const result = await runHook(hook, payload, cwd);
    const output = parseHookOutput(result.stdout);
    const specific = hookSpecific(output);
    const additional = String(specific?.additionalContext ?? output?.additionalContext ?? output?.message ?? result.stderr ?? '').trim();
    if (additional) feedback.push(additional);
  }
  return { feedback };
}
