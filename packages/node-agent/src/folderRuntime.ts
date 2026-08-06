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
