import { createHash, randomBytes } from 'node:crypto';
import type { JsonObject, ToolContext } from '../../types.js';
import { toolNames } from '../../catalog.js';

export const HANDLE_KEY_PREFIX = 'ctmcp-handle:';
export const CONVERSATION_HANDLE_TTL_MS = 86_400_000;
export const MAX_CONVERSATION_HANDLES = 1024;

interface Entry {
  owner: string;
  native?: string;
  key: string;
  expiresAt: number;
}

export interface HandleResponse {
  token?: string;
  expiresAt: number;
  platformBound: boolean;
}

const object = (value: unknown): JsonObject => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : {};
const digest = (value: string): string => createHash('sha256').update(value).digest('hex');

function failure(code: string, message: string): Error {
  return Object.assign(new Error(message), { rpcCode: -32602, rpcData: {
    error_code: code, error_category: 'workspace_routing', retryable: false
  } });
}

function invalid(): Error {
  return failure('CONVERSATION_HANDLE_INVALID', 'Invalid conversation_handle for this authenticated credential and runtime; obtain a new handle with conversation_bootstrap.');
}

export class ConversationHandleStore {
  private readonly entries = new Map<string, Entry>();

  constructor(private readonly forget: (key: string) => void = () => {}) {}

  /** Return only expiry times for the local management UI, never tokens or owners. */
  expiryTimes(now = Date.now()): number[] {
    for (const [id, entry] of this.entries) {
      if (entry.expiresAt > now) continue;
      this.entries.delete(id);
      if (!entry.native) this.forget(entry.key);
    }
    return [...this.entries.values()].map(entry => entry.expiresAt).sort((a, b) => a - b);
  }

  prepare(owner: string | undefined, request: JsonObject, now = Date.now()): HandleResponse | undefined {
    const params = object(request.params);
    const originalMeta = object(params._meta);
    const native = typeof originalMeta['openai/session'] === 'string'
      ? originalMeta['openai/session'].trim() || undefined : undefined;
    if (native?.startsWith(HANDLE_KEY_PREFIX)) {
      throw failure('CONVERSATION_HANDLE_CONFLICT', 'Reserved conversation identity namespace.');
    }
    if (request.method !== 'tools/call') return undefined;
    const name = String(params.name ?? '');
    const args = object(params.arguments);
    const supplied = Object.hasOwn(args, 'conversation_handle');
    const raw = args.conversation_handle;
    const create = args.create_conversation_handle;
    delete args.conversation_handle;
    delete args.create_conversation_handle;
    if (create !== undefined && (name !== 'conversation_bootstrap' || typeof create !== 'boolean')) {
      throw failure('INVALID_ARGUMENT', 'create_conversation_handle is a boolean supported only by conversation_bootstrap.');
    }
    const mint = !supplied && name === 'conversation_bootstrap' && (native === undefined || create === true);
    if (!supplied && !mint) return undefined;
    if (!toolNames.includes(name)) throw failure('INVALID_ARGUMENT', 'conversation_handle is supported only by built-in MCP tools.');
    if (!owner) throw failure('CONVERSATION_HANDLE_AUTH_REQUIRED', 'conversation_handle requires verified Bearer authentication.');
    let token: string;
    let entry: Entry;
    if (supplied) {
      if (typeof raw !== 'string' || !/^ch_[a-f0-9]{64}$/.test(raw)) throw invalid();
      token = raw;
      const existing = this.entries.get(digest(token));
      if (!existing || existing.owner !== owner) throw invalid();
      if (existing.native !== native) throw failure('CONVERSATION_HANDLE_CONFLICT', 'conversation_handle does not match the platform conversation identity used at issuance.');
      if (existing.expiresAt <= now) {
        this.entries.delete(digest(token));
        if (!existing.native) this.forget(existing.key);
        throw failure('CONVERSATION_HANDLE_EXPIRED', 'conversation_handle expired; obtain a new handle with conversation_bootstrap.');
      }
      entry = existing;
    } else {
      for (const [id, existing] of this.entries) {
        if (existing.expiresAt > now) continue;
        this.entries.delete(id);
        if (!existing.native) this.forget(existing.key);
      }
      if (this.entries.size >= MAX_CONVERSATION_HANDLES) {
        throw failure('CONVERSATION_HANDLE_LIMIT', 'Conversation handle capacity reached; reuse an existing handle or wait for expiry.');
      }
      token = `ch_${randomBytes(32).toString('hex')}`;
      entry = { owner, native, key: native ?? `${HANDLE_KEY_PREFIX}${randomBytes(32).toString('hex')}`, expiresAt: now + CONVERSATION_HANDLE_TTL_MS };
      this.entries.set(digest(token), entry);
    }
    const meta: JsonObject = { ...originalMeta, 'openai/session': entry.key };
    if (!native) {
      const capabilities = object(meta['io.modelcontextprotocol/clientCapabilities']);
      const extensions = { ...object(capabilities.extensions) };
      delete extensions['io.modelcontextprotocol/tasks'];
      if (Object.hasOwn(meta, 'io.modelcontextprotocol/clientCapabilities')) {
        meta['io.modelcontextprotocol/clientCapabilities'] = { ...capabilities, extensions };
      }
    }
    params._meta = meta;
    request.params = params;
    return { token: name === 'conversation_bootstrap' ? token : undefined, expiresAt: entry.expiresAt, platformBound: native !== undefined };
  }
}

const stores = new WeakMap<ToolContext, ConversationHandleStore>();

export function conversationHandleExpiryTimes(ctx: ToolContext, now = Date.now()): number[] {
  return stores.get(ctx)?.expiryTimes(now) ?? [];
}

/** Call only after HTTP verification. Never obtain `credential` from JSON. */
export function prepareConversationHandle(ctx: ToolContext, credential: string | undefined, request: JsonObject): HandleResponse | undefined {
  let store = stores.get(ctx);
  if (!store) {
    store = new ConversationHandleStore(key => { ctx.conversations.deleteConversation(key); });
    stores.set(ctx, store);
  }
  return store.prepare(credential ? digest(credential) : undefined, request);
}

/** Attach only after dispatch/usage telemetry. No raw token enters execution. */
export function decorateHandleResult(result: unknown, grant: HandleResponse | undefined): unknown {
  if (!grant || !result || typeof result !== 'object' || Array.isArray(result)) return result;
  const fields: JsonObject = {
    conversation_identity_source: 'conversation_handle',
    platform_conversation_bound: grant.platformBound,
    conversation_handle_expires_at_ms: grant.expiresAt,
    ...(grant.token ? { conversation_handle: grant.token } : {})
  };
  const output = result as JsonObject;
  const structured = output.structuredContent && typeof output.structuredContent === 'object' && !Array.isArray(output.structuredContent)
    ? output.structuredContent as JsonObject : undefined;
  if (!structured && !Array.isArray(output.content)) return output;
  const content = Array.isArray(output.content) ? output.content : [];
  const originalText = content.filter(block => object(block).type === 'text')
    .map(block => object(block).text).filter((value): value is string => typeof value === 'string').join('\n');
  const payload = { ...(structured ?? (originalText ? { text: originalText } : {})), ...fields };
  output.structuredContent = payload;
  // Some clients consume only one text block and ignore structuredContent.
  // Keep the complete payload in that block, preserving images/resources.
  output.content = [{ type: 'text', text: JSON.stringify(payload) }, ...content.filter(block => object(block).type !== 'text')];
  return output;
}

/** Attach verified caller's OAuth expiry to a session_status result, without the credential. */
export function decorateSessionStatus(result: unknown, expiresAtMs: number | null): unknown {
  if (!result || typeof result !== 'object' || Array.isArray(result)) return result;
  const output = result as JsonObject;
  const structured = output.structuredContent;
  if (!structured || typeof structured !== 'object' || Array.isArray(structured)) return result;
  if ((structured as JsonObject).ok === false) return result;
  const payload = { ...(structured as JsonObject), oauth_token_expires_at_ms: expiresAtMs };
  output.structuredContent = payload;
  if (Array.isArray(output.content)) {
    output.content = [
      { type: 'text', text: JSON.stringify(payload) },
      ...output.content.filter(block => object(block).type !== 'text')
    ];
  }
  return output;
}
