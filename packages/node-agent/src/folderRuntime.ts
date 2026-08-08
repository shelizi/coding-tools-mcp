import { deriveWorkspaceProfileId } from './conversation.js';
import { currentExecutionBinding } from './executionScope.js';
import { KeyedMutex, Semaphore } from './runtime.js';
import type { AgentConfig, FolderRuntime, PendingOperation, ToolContext, WorkspaceFolder } from './types.js';

export function createFolderRuntime(config: AgentConfig, folder: WorkspaceFolder): FolderRuntime {
  return {
    folderId: folder.id,
    workspacePath: folder.path,
    sessions: new Map(),
    operationsByFingerprint: new Map(),
    pendingOperations: new Map(),
    editProposals: new Map(),
    admission: {
      blocking: new Semaphore(config.limits.blockingConcurrency),
      process: new Semaphore(config.limits.processConcurrency),
      locks: new KeyedMutex()
    }
  };
}

export interface WorkspaceFolderHotApplyResult {
  changed: boolean;
  applied: boolean;
  deferredReason?: string;
}

function folderRuntimeBusy(runtime: FolderRuntime): boolean {
  return [...runtime.sessions.values()].some(session => !session.finalizedAt)
    || runtime.pendingOperations.size > 0
    || runtime.editProposals.size > 0;
}

function sameFolderConfiguration(left: readonly WorkspaceFolder[], right: readonly WorkspaceFolder[]): boolean {
  return left.length === right.length && left.every((folder, index) => {
    const candidate = right[index];
    return candidate?.id === folder.id && candidate.name === folder.name && candidate.path === folder.path;
  });
}

export function applyWorkspaceFolderConfiguration(
  ctx: ToolContext,
  folders: readonly WorkspaceFolder[]
): WorkspaceFolderHotApplyResult {
  if (sameFolderConfiguration(ctx.config.folders, folders)) return { changed: false, applied: false };

  const desiredById = new Map(folders.map(folder => [folder.id, folder]));
  for (const [folderId, runtime] of ctx.folderRuntimes) {
    const desired = desiredById.get(folderId);
    if ((!desired || desired.path !== runtime.workspacePath) && folderRuntimeBusy(runtime)) {
      return {
        changed: true,
        applied: false,
        deferredReason: `Workspace folder ${folderId} is still in use by an active session or pending operation.`
      };
    }
  }

  const removedFolder = [...ctx.folderRuntimes.keys()].some(folderId => !desiredById.has(folderId));
  const pathChanged = folders.some(folder => {
    const runtime = ctx.folderRuntimes.get(folder.id);
    return Boolean(runtime && runtime.workspacePath !== folder.path);
  });
  const nextRuntimes = new Map<string, FolderRuntime>();
  for (const folder of folders) {
    const runtime = ctx.folderRuntimes.get(folder.id) ?? createFolderRuntime(ctx.config, folder);
    if (runtime.workspacePath !== folder.path) {
      runtime.workspacePath = folder.path;
      runtime.operationsByFingerprint.clear();
    }
    nextRuntimes.set(folder.id, runtime);
  }

  ctx.folderRuntimes.clear();
  for (const [folderId, runtime] of nextRuntimes) ctx.folderRuntimes.set(folderId, runtime);
  ctx.config.folders = folders.map(folder => ({ ...folder }));
  if (!ctx.config.workspaceId) ctx.workspaceProfileId = deriveWorkspaceProfileId(ctx.config.folders);

  const firstRuntime = ctx.folderRuntimes.values().next().value;
  if (!firstRuntime) throw new Error('at least one workspace folder is required');
  ctx.sessions = firstRuntime.sessions;
  ctx.operationsByFingerprint = firstRuntime.operationsByFingerprint;
  ctx.pendingOperations = firstRuntime.pendingOperations;
  ctx.editProposals = firstRuntime.editProposals;
  ctx.admission = firstRuntime.admission;

  if (removedFolder) ctx.selections.clear();
  else if (pathChanged) ctx.defaultCwds.clear();
  return { changed: true, applied: true };
}

export function runtimeForFolderId(ctx: ToolContext, folderId: string): FolderRuntime {
  const runtime = ctx.folderRuntimes.get(folderId);
  if (!runtime) throw new Error('WORKSPACE_FOLDER_NOT_FOUND');
  return runtime;
}

export function currentFolderRuntime(ctx: ToolContext, key: string): FolderRuntime {
  const binding = currentExecutionBinding(ctx, key);
  if (binding?.runtime) return binding.runtime;
  const folderId = binding?.folderId ?? ctx.selections.get(key);
  if (!folderId) throw new Error('WORKSPACE_FOLDER_NOT_SELECTED');
  return runtimeForFolderId(ctx, folderId);
}

export function allFolderRuntimes(ctx: ToolContext): FolderRuntime[] {
  return [...ctx.folderRuntimes.values()];
}

export function findPendingOperation(
  ctx: ToolContext,
  resumeId: string
): { runtime: FolderRuntime; operation: PendingOperation } | undefined {
  for (const runtime of ctx.folderRuntimes.values()) {
    const operation = runtime.pendingOperations.get(resumeId);
    if (operation) return { runtime, operation };
  }
  return undefined;
}
