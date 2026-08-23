import type { OperationRecord, ProcessSession, ToolContext } from '../types.js';
import { allFolderRuntimes } from '../folderRuntime.js';
import { operationResultSummary } from '../operationSummary.js';
import { processResult } from './output.js';

export async function recordHarnessOperationFinalization(
  ctx: ToolContext,
  session: ProcessSession
): Promise<void> {
  if (!session.finalizedAt || !session.harnessOperations?.size) return;
  const recordedIds = session.harnessOperationRecordedIds ??= new Set<string>();
  for (const operation of session.harnessOperations.values()) {
    if (recordedIds.has(operation.id)) continue;
    recordedIds.add(operation.id);
    const summary = operationResultSummary(operation.tool, processResult(session, { output_mode: 'none' }));
    const completed: OperationRecord = {
      ...operation,
      kind: summary.command_ok === true ? 'completed' : 'failed',
      result_summary: summary,
      created_at: String(session.finalizedAt)
    };
    try {
      await ctx.state.addOperation(operation.workspace_id, completed);
    } catch {
      recordedIds.delete(operation.id);
    }
  }
}

export async function attachHarnessOperation(
  ctx: ToolContext,
  sessionId: string,
  operation: OperationRecord
): Promise<boolean> {
  const session = allFolderRuntimes(ctx)
    .map(runtime => runtime.sessions.get(sessionId))
    .find((candidate): candidate is ProcessSession => Boolean(candidate));
  if (!session) return false;
  const operations = session.harnessOperations ??= new Map<string, OperationRecord>();
  operations.set(operation.id, operation);
  if (session.finalizedAt) await recordHarnessOperationFinalization(ctx, session);
  return true;
}
