import { createHash, randomUUID } from 'node:crypto';
import { mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import type { JsonObject, WorkspaceFolder } from './types.js';

export const MAX_CONVERSATION_CONTEXTS = 128;
export const MAX_PERSISTED_CONVERSATION_CONTEXTS = MAX_CONVERSATION_CONTEXTS * 4;
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

interface PersistedConversationContext {
  conversationKey: string;
  folderId: string;
  cwd: string;
  lastUsed: number;
}

interface ConversationSnapshot {
  version: 1;
  selections: Array<[string, string]>;
  contexts: PersistedConversationContext[];
}

function validPersistedConversationKey(value: unknown): value is string {
  return typeof value === 'string' && value.length > 0 && value.length <= 512 && !value.includes('\0');
}

function validPersistedCwd(value: unknown): value is string {
  if (typeof value !== 'string' || value.length === 0 || value.length > 4096 || value.includes('\0') || path.isAbsolute(value)) return false;
  return !value.split(/[\\/]+/).some(part => part === '..');
}

export class ConversationStore {
  private readonly selections = new Map<string, string>();
  private readonly activeContexts = new Map<string, ActiveConversationContext>();
  private readonly savedCwds = new Map<string, SavedConversationCwd>();
  private readonly allowedFolderIds?: ReadonlySet<string>;
  private accessClock = 0;
  private persistDirty = false;
  private persistQueued = false;
  private persistTail: Promise<void> = Promise.resolve();
  readonly fallbackKey: string;
  readonly missingMcpKey: string;

  constructor(
    readonly capacity = MAX_CONVERSATION_CONTEXTS,
    fallbackKey = `runtime-fallback:${randomUUID()}`,
    private readonly persistencePath?: string,
    allowedFolderIds?: Iterable<string>
  ) {
    if (!Number.isInteger(capacity) || capacity < 1) throw new Error('conversation context capacity must be positive');
    this.fallbackKey = fallbackKey;
    this.missingMcpKey = `${fallbackKey}:missing-mcp-conversation`;
    this.allowedFolderIds = allowedFolderIds ? new Set(allowedFolderIds) : undefined;
  }

  static async open(options: {
    capacity?: number;
    fallbackKey?: string;
    persistencePath?: string;
    allowedFolderIds?: Iterable<string>;
  } = {}): Promise<ConversationStore> {
    const store = new ConversationStore(
      options.capacity ?? MAX_CONVERSATION_CONTEXTS,
      options.fallbackKey,
      options.persistencePath,
      options.allowedFolderIds
    );
    await store.restore();
    return store;
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
    const cwd = this.ensureContext(conversationKey, folderId).cwd;
    this.schedulePersist();
    return cwd;
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
    this.schedulePersist();
  }

  deleteConversation(conversationKey: string): boolean {
    const existed = this.selections.delete(conversationKey);
    for (const [key, context] of this.activeContexts) {
      if (context.conversationKey === conversationKey) this.activeContexts.delete(key);
    }
    for (const key of this.savedCwds.keys()) {
      if (key.endsWith(`\0${conversationKey}`)) this.savedCwds.delete(key);
    }
    if (existed) this.schedulePersist();
    return existed;
  }

  clear(): void {
    this.selections.clear();
    this.activeContexts.clear();
    this.savedCwds.clear();
    this.schedulePersist();
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

  async flush(): Promise<void> {
    if (!this.persistencePath) return;
    this.persistQueued = false;
    this.enqueuePersist();
    await this.persistTail;
  }

  private async restore(): Promise<void> {
    if (!this.persistencePath) return;
    let snapshot: ConversationSnapshot;
    try {
      snapshot = JSON.parse(await readFile(this.persistencePath, 'utf8')) as ConversationSnapshot;
    } catch {
      return;
    }
    if (snapshot?.version !== 1 || !Array.isArray(snapshot.contexts) || !Array.isArray(snapshot.selections)) return;
    for (const item of snapshot.contexts.slice(0, MAX_PERSISTED_CONVERSATION_CONTEXTS)) {
      if (!item || !validPersistedConversationKey(item.conversationKey) || !validPersistedConversationKey(item.folderId)) continue;
      if (this.allowedFolderIds && !this.allowedFolderIds.has(item.folderId)) continue;
      if (!validPersistedCwd(item.cwd)) continue;
      const lastUsed = Number.isFinite(item.lastUsed) && item.lastUsed >= 0 ? Math.floor(item.lastUsed) : 0;
      this.savedCwds.set(contextKey(item.conversationKey, item.folderId), { cwd: item.cwd, lastUsed });
      this.accessClock = Math.max(this.accessClock, lastUsed);
    }
    for (const selection of snapshot.selections.slice(0, MAX_PERSISTED_CONVERSATION_CONTEXTS)) {
      if (!Array.isArray(selection) || selection.length !== 2) continue;
      const [conversationKey, folderId] = selection;
      if (!validPersistedConversationKey(conversationKey) || !validPersistedConversationKey(folderId)) continue;
      if (this.allowedFolderIds && !this.allowedFolderIds.has(folderId)) continue;
      this.selections.set(conversationKey, folderId);
    }
  }

  private schedulePersist(): void {
    if (!this.persistencePath) return;
    this.persistDirty = true;
    if (this.persistQueued) return;
    this.persistQueued = true;
    queueMicrotask(() => {
      this.persistQueued = false;
      this.enqueuePersist();
    });
  }

  private enqueuePersist(): void {
    if (!this.persistencePath || !this.persistDirty) return;
    this.persistDirty = false;
    const snapshot = this.snapshot();
    const write = () => this.writeSnapshot(snapshot);
    this.persistTail = this.persistTail.then(write, write).catch(() => {});
  }

  private snapshot(): ConversationSnapshot {
    const contexts = [
      ...[...this.savedCwds.entries()].map(([key, saved]) => {
        const separator = key.indexOf('\0');
        return {
          conversationKey: key.slice(separator + 1),
          folderId: key.slice(0, separator),
          cwd: saved.cwd,
          lastUsed: saved.lastUsed
        };
      }),
      ...[...this.activeContexts.values()].map(context => ({
        conversationKey: context.conversationKey,
        folderId: context.folderId,
        cwd: context.cwd,
        lastUsed: context.lastUsed
      }))
    ]
      .sort((left, right) => right.lastUsed - left.lastUsed || left.folderId.localeCompare(right.folderId) || left.conversationKey.localeCompare(right.conversationKey))
      .slice(0, MAX_PERSISTED_CONVERSATION_CONTEXTS);
    const retained = new Set(contexts.map(context => contextKey(context.conversationKey, context.folderId)));
    const selections = [...this.selections.entries()]
      .filter(([conversationKey, folderId]) => retained.has(contextKey(conversationKey, folderId)))
      .slice(-MAX_PERSISTED_CONVERSATION_CONTEXTS);
    return { version: 1, selections, contexts };
  }

  private async writeSnapshot(snapshot: ConversationSnapshot): Promise<void> {
    if (!this.persistencePath) return;
    await mkdir(path.dirname(this.persistencePath), { recursive: true });
    const temporary = `${this.persistencePath}.${process.pid}.${randomUUID()}.tmp`;
    await writeFile(temporary, `${JSON.stringify(snapshot)}\n`, 'utf8');
    try {
      await rename(temporary, this.persistencePath);
    } catch {
      await rm(this.persistencePath, { force: true });
      await rename(temporary, this.persistencePath);
    } finally {
      await rm(temporary, { force: true });
    }
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
