import { AsyncLocalStorage } from 'node:async_hooks';
import type { FolderRuntime, ToolContext } from './types.js';

export interface ToolExecutionBinding {
  ctx: ToolContext;
  key: string;
  folderId?: string;
  defaultCwd?: string;
  runtime?: FolderRuntime;
  requestedWorkspaceId?: string;
  selectedWorkspaceId?: string;
  routeSource?: 'conversation' | 'explicit' | 'session_id' | 'resume_id' | 'permission_resume';
}

const executionBindings = new AsyncLocalStorage<ToolExecutionBinding>();

export function runWithExecutionBinding<T>(binding: ToolExecutionBinding, callback: () => T): T {
  return executionBindings.run(binding, callback);
}

export function currentExecutionBinding(ctx: ToolContext, key: string): ToolExecutionBinding | undefined {
  const binding = executionBindings.getStore();
  if (!binding || binding.key !== key) return undefined;
  const sameContext = binding.ctx === ctx
    || (
      binding.ctx.conversations === ctx.conversations
      && binding.ctx.folderRuntimes === ctx.folderRuntimes
      && binding.ctx.hubAdmission === ctx.hubAdmission
    );
  return sameContext ? binding : undefined;
}
