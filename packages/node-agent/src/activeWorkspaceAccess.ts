import { readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import type { ConversationStoreContract } from './conversation/contract.js';
import type { JsonObject, ToolContext, WorkspaceFolder } from './types.js';

type CheckoutKind = 'worktree' | 'repository' | 'folder';

interface AccessRecord {
  conversationKey: string;
  folderId: string;
  checkoutPath: string;
  checkoutKind: CheckoutKind;
  lastAccessMs: number;
  toolCalls: number;
}

// Never infer liveness from saved folder selections. Only recent calls in the
// current process count. Keep up to one hour of metadata for custom queries;
// the default active/recent windows remain three and five minutes.
const RETENTION_MS = 60 * 60_000;
const MAX_RECORDS = 1024;
const accessByStore = new WeakMap<ConversationStoreContract, Map<string, AccessRecord>>();

function recordsFor(store: ConversationStoreContract): Map<string, AccessRecord> {
  let records = accessByStore.get(store);
  if (!records) {
    records = new Map();
    accessByStore.set(store, records);
  }
  return records;
}

function inside(root: string, candidate: string): boolean {
  const relative = path.relative(root, candidate);
  return relative === '' || (relative !== '..' && !relative.startsWith('..' + path.sep) && !path.isAbsolute(relative));
}

function detectCheckout(folder: WorkspaceFolder, name: string, args: JsonObject): { checkoutPath: string; checkoutKind: CheckoutKind } {
  const root = path.resolve(folder.path);
  const raw = name.startsWith('git_') ? (args.repo_path ?? args.path)
    : name === 'exec_command' ? args.workdir
      : (args.path ?? args.workdir);
  let location = typeof raw === 'string' && raw.trim() ? path.resolve(root, raw) : root;
  if (!inside(root, location)) location = root;
  try {
    if (!statSync(location).isDirectory()) location = path.dirname(location);
  } catch {
    location = root;
  }
  while (inside(root, location)) {
    try {
      const marker = statSync(path.join(location, '.git'));
      if (marker.isFile()) {
        const gitdir = readFileSync(path.join(location, '.git'), 'utf8').replaceAll('\\', '/');
        return { checkoutPath: location, checkoutKind: gitdir.includes('/modules/') ? 'repository' : 'worktree' };
      }
      if (marker.isDirectory()) return { checkoutPath: location, checkoutKind: 'repository' };
    } catch {
      // Keep searching toward the configured workspace root.
    }
    if (location === root) break;
    location = path.dirname(location);
  }
  return { checkoutPath: root, checkoutKind: 'folder' };
}

function prune(records: Map<string, AccessRecord>, now: number): void {
  for (const [key, entry] of records) {
    if (now - entry.lastAccessMs > RETENTION_MS) records.delete(key);
  }
  if (records.size <= MAX_RECORDS) return;
  const oldest = [...records.entries()].sort((a, b) => a[1].lastAccessMs - b[1].lastAccessMs);
  for (const [key] of oldest.slice(0, records.size - MAX_RECORDS)) records.delete(key);
}

export function noteWorkspaceToolAccess(
  ctx: ToolContext, conversationKey: string, folderId: string | undefined,
  name: string, args: JsonObject
): void {
  if (!folderId || [
    'list_workspace_folders', 'list_active_workspaces', 'server_info',
    'query_tool_usage', 'conversation_bootstrap', 'switch_workspace_folder'
  ].includes(name)) return;
  const folder = ctx.config.folders.find(item => item.id === folderId);
  if (!folder) return;
  const now = Date.now();
  const checkout = detectCheckout(folder, name, args);
  const records = recordsFor(ctx.conversations);
  const key = conversationKey + '\0' + folderId + '\0' + checkout.checkoutPath;
  const existing = records.get(key);
  if (existing) {
    existing.lastAccessMs = now;
    existing.toolCalls++;
  } else {
    records.set(key, { conversationKey, folderId, ...checkout, lastAccessMs: now, toolCalls: 1 });
  }
  prune(records, now);
}

export function listActiveWorkspaces(ctx: ToolContext, args: JsonObject): JsonObject {
  const activeMinutes = args.active_within_minutes ?? 3;
  const recentMinutes = args.recent_within_minutes ?? 5;
  if (!Number.isInteger(activeMinutes) || !Number.isInteger(recentMinutes)
    || Number(activeMinutes) < 1 || Number(recentMinutes) > 60
    || Number(recentMinutes) < Number(activeMinutes)) {
    return {
      ok: false,
      error: {
        code: 'INVALID_ARGUMENT',
        category: 'validation',
        retryable: false,
        message: 'active_within_minutes and recent_within_minutes must be integers from 1 to 60, with recent >= active'
      }
    };
  }
  const now = Date.now();
  const records = recordsFor(ctx.conversations);
  prune(records, now);
  const activeCutoff = now - Number(activeMinutes) * 60_000;
  const recentCutoff = now - Number(recentMinutes) * 60_000;
  const projects = ctx.config.folders.flatMap(folder => {
    const entries = [...records.values()].filter(item => item.folderId === folder.id && item.lastAccessMs >= recentCutoff);
    if (!entries.length) return [];
    const groups = new Map<string, AccessRecord[]>();
    for (const entry of entries) {
      const group = groups.get(entry.checkoutPath) ?? [];
      group.push(entry);
      groups.set(entry.checkoutPath, group);
    }
    const checkouts = [...groups.values()].map(group => {
      const lastAccessMs = Math.max(...group.map(item => item.lastAccessMs));
      return {
        path: group[0]!.checkoutPath,
        kind: group[0]!.checkoutKind,
        status: lastAccessMs >= activeCutoff ? 'active' : 'recent',
        conversation_count: new Set(group.map(item => item.conversationKey)).size,
        tool_call_count: group.reduce((sum, item) => sum + item.toolCalls, 0),
        last_access_at_ms: lastAccessMs
      };
    }).sort((a, b) => b.last_access_at_ms - a.last_access_at_ms);
    return [{
      folder_id: folder.id,
      name: folder.name,
      path: folder.path,
      status: checkouts.some(item => item.status === 'active') ? 'active' : 'recent',
      conversation_count: new Set(entries.map(item => item.conversationKey)).size,
      tool_call_count: entries.reduce((sum, item) => sum + item.toolCalls, 0),
      last_access_at_ms: Math.max(...entries.map(item => item.lastAccessMs)),
      checkouts
    }];
  }).sort((a, b) => b.last_access_at_ms - a.last_access_at_ms);
  return {
    ok: true,
    observed_at_ms: now,
    active_within_minutes: activeMinutes,
    recent_within_minutes: recentMinutes,
    active_project_count: projects.filter(item => item.status === 'active').length,
    recent_project_count: projects.filter(item => item.status === 'recent').length,
    active_worktree_count: projects.flatMap(item => item.checkouts).filter(item => item.kind === 'worktree' && item.status === 'active').length,
    projects,
    activity_definition: 'Successful tool use observed by this MCP runtime; recent activity is not a filesystem lock or a live-agent heartbeat.'
  };
}
