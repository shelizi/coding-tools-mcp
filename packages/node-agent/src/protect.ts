import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const WRAPPED_KIND = 'ctmcp-wrap';

function protectBin(): string | undefined {
  const configured = process.env.CTMCP_PROTECT_BIN?.trim();
  if (configured) return configured;
  const local = fileURLToPath(new URL('./ctmcp-protect.exe', import.meta.url));
  if (existsSync(local)) return local;
  const unextended = fileURLToPath(new URL('./ctmcp-protect', import.meta.url));
  if (existsSync(unextended)) return unextended;
  for (const candidate of [
    '../../../src-tauri/target/debug/ctmcp-protect.exe',
    '../../../src-tauri/target/release/ctmcp-protect.exe',
    '../../../src-tauri/target/debug/ctmcp-protect',
    '../../../src-tauri/target/release/ctmcp-protect'
  ]) {
    const binary = fileURLToPath(new URL(candidate, import.meta.url));
    if (existsSync(binary)) return binary;
  }
  return undefined;
}

export function protectAvailable(): boolean {
  return protectBin() !== undefined;
}

function runProtect(command: 'wrap' | 'unwrap', associatedPath: string, value: unknown): unknown {
  const bin = protectBin();
  if (!bin) {
    throw new Error('Rust workspace protect helper is unavailable (set CTMCP_PROTECT_BIN)');
  }
  const result = spawnSync(bin, [command, associatedPath], {
    input: JSON.stringify(value),
    encoding: 'utf8',
    windowsHide: true
  });
  if (result.status !== 0) {
    throw new Error(result.stderr?.trim() || `ctmcp-protect ${command} failed`);
  }
  return JSON.parse(result.stdout);
}

export function isWrappedDocument(value: unknown): boolean {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value)
    && (value as { kind?: string }).kind === WRAPPED_KIND;
}

export function wrapJson(value: unknown, associatedPath: string): unknown {
  return runProtect('wrap', associatedPath, value);
}

export function unwrapJson(value: unknown, associatedPath: string): unknown {
  if (!isWrappedDocument(value)) return value;
  return runProtect('unwrap', associatedPath, value);
}
