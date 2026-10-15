import { createHash } from 'node:crypto';
import type { IncomingHttpHeaders } from 'node:http';
import { redactSensitiveText } from '../../redaction.js';
import type { JsonObject, ToolContext } from '../../types.js';

export const IDENTITY_DIAGNOSTICS_ENV = 'CTMCP_MCP_IDENTITY_DIAGNOSTICS';
const MAX_KEYS = 64;

function object(value: unknown): JsonObject {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : {};
}

function label(value: unknown): string | null {
  if (typeof value !== 'string') return null;
  return [...redactSensitiveText(value).value].filter(ch => !/[\x00-\x1f\x7f]/.test(ch)).slice(0, 128).join('');
}

function identityKey(key: string): boolean {
  key = key.toLowerCase();
  return !['auth', 'cookie', 'token', 'secret', 'password', 'passwd', 'credential', 'api-key', 'api_key', 'apikey', 'private']
    .some(word => key.includes(word))
    && ['session', 'conversation', 'thread', 'chat'].some(word => key.includes(word));
}

function fingerprint(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  value = value.trim();
  return value && Buffer.byteLength(value as string) <= 4096
    ? createHash('sha256').update(value as string).digest('hex') : undefined;
}

export function identitySnapshot(headers: IncomingHttpHeaders, body: JsonObject): JsonObject {
  const params = object(body.params);
  const meta = object(params._meta);
  const headerKeys = Object.keys(headers).sort();
  const metaKeys = Object.keys(meta).sort();
  const candidates: JsonObject[] = [];
  for (const [source, values, keys] of [
    ['header', headers, headerKeys], ['meta', meta, metaKeys]
  ] as const) {
    for (const key of keys.slice(0, MAX_KEYS)) {
      if (!identityKey(key)) continue;
      const hash = fingerprint(values[key]);
      if (hash) candidates.push({ source, key: label(key), sha256: hash });
    }
  }
  const clientInfo = object(params.clientInfo ?? meta['io.modelcontextprotocol/clientInfo']);
  return {
    method: label(body.method), tool: label(params.name),
    header_keys: headerKeys.slice(0, MAX_KEYS).map(label),
    meta_keys: metaKeys.slice(0, MAX_KEYS).map(label),
    keys_truncated: headerKeys.length > MAX_KEYS || metaKeys.length > MAX_KEYS,
    identity_candidates: candidates,
    client_name: label(clientInfo.name), client_version: label(clientInfo.version)
  };
}

export function recordIdentityDiagnostics(ctx: ToolContext, headers: IncomingHttpHeaders, body: JsonObject): void {
  if (process.env[IDENTITY_DIAGNOSTICS_ENV] !== '1') return;
  ctx.usageStore.recordDiagnosticEvent({
    eventType: 'transport_event', event: 'mcp_identity_observed', severity: 'info', failureDomain: 'none',
    fields: identitySnapshot(headers, body)
  });
}
