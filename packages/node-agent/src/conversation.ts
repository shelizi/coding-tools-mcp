import { createHash, randomUUID } from 'node:crypto';
import type { JsonObject, WorkspaceFolder } from './types.js';

export const MAX_CONVERSATION_CONTEXTS = 128;
export const FALLBACK_CONVERSATION_KEY = '__ctmcp_stable_unscoped__';
export const MCP_CONVERSATION_REQUIRED_META = 'coding-tools/require-conversation';

export interface ConversationIdentity {
  key: string;
  isolated: boolean;
  requiresConversation: boolean;
  source: 'platform_conversation_id' | 'stable_fallback' | 'missing_mcp_conversation';
}

export interface MutableStringMap {
  get(key: string): string | undefined;
  set(key: string, value: string): this;
  has(key: string): boolean;
  delete(key: string): boolean;
  clear(): void;
  readonly size: number;
}

export class ConversationRoutingError extends Error {
  readonly category = 'workspace_routing';

  constructor(
    readonly code: string,
    message: string,
    readonly retryable = false,
    readonly details: JsonObject = {}
  ) {
    super(message);
    this.name = 'ConversationRoutingError';
  }
}

function metadataRecord(meta: unknown): Record<string, unknown> {
  return meta && typeof meta === 'object' && !Array.isArray(meta)
    ? meta as Record<string, unknown>
    : {};
}

export function markMcpConversationMetadata(meta: unknown): Record<string, unknown> {
  return {
    ...metadataRecord(meta),
    [MCP_CONVERSATION_REQUIRED_META]: true
  };
}

export function conversationIdentity(
  meta: unknown,
  fallbackKey = FALLBACK_CONVERSATION_KEY,
  missingMcpKey = `${fallbackKey}:missing-mcp-conversation`
): ConversationIdentity {
  const record = metadataRecord(meta);
  const value = String(record['openai/session'] ?? '').trim();
  const requiresConversation = record[MCP_CONVERSATION_REQUIRED_META] === true;
  if (value) {
    return {
      key: value,
      isolated: true,
      requiresConversation,
      source: 'platform_conversation_id'
    };
  }
  if (requiresConversation) {
    return {
      key: missingMcpKey,
      isolated: false,
      requiresConversation: true,
      source: 'missing_mcp_conversation'
    };
  }
  return {
    key: fallbackKey,
    isolated: false,
    requiresConversation: false,
    source: 'stable_fallback'
  };
}

export function deriveWorkspaceProfileId(folders: readonly WorkspaceFolder[]): string {
  const identity = folders.map(folder => ({ id: folder.id, path: folder.path }));
  const digest = createHash('sha256').update(JSON.stringify(identity)).digest('hex').slice(0, 16);
  return `node-${digest}`;
}

interface ActiveConversationContext {
  conversationKey: string;
  folderId: string;
  cwd: string;
  lastUsed: number;
}

interface SavedConversationCwd {
  cwd: string;
  lastUsed: number;
}

function contextKey(conversationKey: string, folderId: string): string {
  return `${folderId}\0${conversationKey}`;
}

export class ConversationStore {
  private readonly selections = new Map<string, string>();
  private readonly activeContexts = new Map<string, ActiveConversationContext>();
  private readonly savedCwds = new Map<string, SavedConversationCwd>();
  private accessClock = 0;
  readonly fallbackKey: string;
  readonly missingMcpKey: string;

  constructor(
    readonly capacity = MAX_CONVERSATION_CONTEXTS,
    fallbackKey = `runtime-fallback:${randomUUID()}`
  ) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new Error('conversation context capacity must be positive');
    this.fallbackKey = fallbackKey;
    this.missingMcpKey = `${fallbackKey}:missing-mcp-conversation`;
  }

  readonly selectionMap: MutableStringMap = new ConversationSelectionMap(this);
  readonly cwdMap: MutableStringMap = new ConversationCwdMap(this);

  identity(meta: unknown): ConversationIdentity {
    return conversationIdentity(meta, this.fallbackKey, this.missingMcpKey);
  }

  selectedFolder(conversationKey: string): string | undefined {
    return this.selections.get(conversationKey);
  }

  selectFolder(conversationKey: string, folderId: string): string {
    this.selections.set(conversationKey, folderId);
    return this.ensureContext(conversationKey, folderId).cwd;
  }

  currentCwd(conversationKey: string): string | undefined {
    const folderId = this.selections.get(conversationKey);
    return folderId ? this.ensureContext(conversationKey, folderId).cwd : undefined;
  }

  cwdFor(conversationKey: string, folderId: string): string {
    return this.ensureContext(conversationKey, folderId).cwd;
  }

  peekCwdFor(conversationKey: string, folderId: string): string {
    const key = contextKey(conversationKey, folderId);
    return this.activeContexts.get(key)?.cwd ?? this.savedCwds.get(key)?.cwd ?? '.';
  }

  setCurrentCwd(conversationKey: string, cwd: string): void {
    const folderId = this.selections.get(conversationKey);
    if (!folderId) {
      throw new ConversationRoutingError(
        'WORKSPACE_FOLDER_NOT_SELECTED',
        'This conversation has not selected a workspace folder. Call list_workspace_folders and switch_workspace_folder first.',
        true
      );
    }
    this.setFolderCwd(conversationKey, folderId, cwd);
  }

  setFolderCwd(conversationKey: string, folderId: string, cwd: string): void {
    const context = this.ensureContext(conversationKey, folderId);
    context.cwd = cwd;
    context.lastUsed = this.nextAccess();
  }

  deleteConversation(conversationKey: string): boolean {
    const existed = this.selections.delete(conversationKey);
    for (const [key, context] of this.activeContexts) {
      if (context.conversationKey === conversationKey) this.activeContexts.delete(key);
    }
    for (const key of this.savedCwds.keys()) {
      if (key.endsWith(`\0${conversationKey}`)) this.savedCwds.delete(key);
    }
    return existed;
  }

  clear(): void {
    this.selections.clear();
    this.activeContexts.clear();
    this.savedCwds.clear();
  }

  hasConversation(conversationKey: string): boolean {
    return this.selections.has(conversationKey);
  }

  get selectionCount(): number {
    return this.selections.size;
  }

  get activeContextCount(): number {
    return this.activeContexts.size;
  }

  get savedCwdCount(): number {
    return this.savedCwds.size;
  }

  isContextActive(conversationKey: string, folderId: string): boolean {
    return this.activeContexts.has(contextKey(conversationKey, folderId));
  }

  isCwdSaved(conversationKey: string, folderId: string): boolean {
    return this.savedCwds.has(contextKey(conversationKey, folderId));
  }

  selectionEntries(): Array<[string, string]> {
    return [...this.selections.entries()];
  }

  currentCwdEntries(): Array<[string, string]> {
    return [...this.selections].map(([conversationKey, folderId]) => [
      conversationKey,
      this.peekCwdFor(conversationKey, folderId)
    ]);
  }

  private ensureContext(conversationKey: string, folderId: string): ActiveConversationContext {
    const key = contextKey(conversationKey, folderId);
    const existing = this.activeContexts.get(key);
    if (existing) {
      existing.lastUsed = this.nextAccess();
      return existing;
    }
    const saved = this.savedCwds.get(key);
    if (saved) this.savedCwds.delete(key);
    const created: ActiveConversationContext = {
      conversationKey,
      folderId,
      cwd: saved?.cwd ?? '.',
      lastUsed: this.nextAccess()
    };
    this.activeContexts.set(key, created);
    this.pruneActiveContexts();
    return created;
  }

  private nextAccess(): number {
    this.accessClock += 1;
    return this.accessClock;
  }

  private pruneActiveContexts(): void {
    const removeCount = this.activeContexts.size - this.capacity;
    if (removeCount > 0) {
      const candidates = [...this.activeContexts.entries()]
        .sort(([leftKey, left], [rightKey, right]) => left.lastUsed - right.lastUsed || leftKey.localeCompare(rightKey));
      for (const [key, context] of candidates.slice(0, removeCount)) {
        this.savedCwds.set(key, { cwd: context.cwd, lastUsed: this.nextAccess() });
        this.activeContexts.delete(key);
      }
    }
  }
}

class ConversationSelectionMap implements MutableStringMap {
  constructor(private readonly store: ConversationStore) {}
  get(key: string): string | undefined { return this.store.selectedFolder(key); }
  set(key: string, value: string): this { this.store.selectFolder(key, value); return this; }
  has(key: string): boolean { return this.store.hasConversation(key); }
  delete(key: string): boolean { return this.store.deleteConversation(key); }
  clear(): void { this.store.clear(); }
  get size(): number { return this.store.selectionCount; }
}

class ConversationCwdMap implements MutableStringMap {
  constructor(private readonly store: ConversationStore) {}
  get(key: string): string | undefined { return this.store.currentCwd(key); }
  set(key: string, value: string): this { this.store.setCurrentCwd(key, value); return this; }
  has(key: string): boolean { return this.store.currentCwd(key) !== undefined; }
  delete(key: string): boolean {
    if (!this.store.hasConversation(key)) return false;
    const folderId = this.store.selectedFolder(key)!;
    this.store.setFolderCwd(key, folderId, '.');
    return true;
  }
  clear(): void {
    for (const [key] of this.store.selectionEntries()) this.delete(key);
  }
  get size(): number { return this.store.selectionCount; }
}
