import { AsyncLocalStorage } from 'node:async_hooks';
import type { FolderRuntime, ToolContext } from './types.js';

export interface ToolExecutionBinding {
  ctx: ToolContext;
  key: string;
  folderId?: string;
  defaultCwd?: string;
  runtime?: FolderRuntime;
}

const executionBindings = new AsyncLocalStorage<ToolExecutionBinding>();

export function runWithExecutionBinding<T>(binding: ToolExecutionBinding, callback: () => T): T {
  return executionBindings.run(binding, callback);
}

export function currentExecutionBinding(ctx: ToolContext, key: string): ToolExecutionBinding | undefined {
  const binding = executionBindings.getStore();
  return binding?.ctx === ctx && binding.key === key ? binding : undefined;
}
