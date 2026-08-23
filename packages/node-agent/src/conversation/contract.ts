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

export interface ConversationStoreContract {
  readonly fallbackKey: string;
  readonly missingMcpKey: string;
  readonly selectionMap: MutableStringMap;
  readonly cwdMap: MutableStringMap;
  identity(meta: unknown): ConversationIdentity;
  selectedFolder(conversationKey: string): string | undefined;
  selectFolder(conversationKey: string, folderId: string): string;
  currentCwd(conversationKey: string): string | undefined;
  cwdFor(conversationKey: string, folderId: string): string;
  peekCwdFor(conversationKey: string, folderId: string): string;
  setCurrentCwd(conversationKey: string, cwd: string): void;
  setFolderCwd(conversationKey: string, folderId: string, cwd: string): void;
  deleteConversation(conversationKey: string): boolean;
  clear(): void;
  hasConversation(conversationKey: string): boolean;
  readonly selectionCount: number;
  readonly activeContextCount: number;
  readonly savedCwdCount: number;
  isContextActive(conversationKey: string, folderId: string): boolean;
  isCwdSaved(conversationKey: string, folderId: string): boolean;
  selectionEntries(): Array<[string, string]>;
  currentCwdEntries(): Array<[string, string]>;
  flush(): Promise<void>;
}
