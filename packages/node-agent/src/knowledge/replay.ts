import type { FolderRuntime } from '../types.js';
import type { ToolUsageJsonObject, ToolUsageStoreContract } from '../toolUsage/contract.js';

const REPLAY_BATCH_SIZE = 500;

export interface KnowledgeReplayWorkspaceResult {
  workspaceId: string;
  sourceEventsSeen: number;
  ledgerEventsAdded: number;
  sourceEventsDeduped: number;
  invalidEvents: number;
  materializedEvents: number;
  materializedRecords: number;
  ignoredEvents: number;
}

export interface KnowledgeReplayResult {
  sourceRevision: string;
  scannedLines: number;
  invalidLines: number;
  duplicateRecordsIgnored: number;
  uniqueRecords: number;
  workspaces: KnowledgeReplayWorkspaceResult[];
}

interface ReplayAccumulator {
  runtime: FolderRuntime;
  buffer: ToolUsageJsonObject[];
  sourceEventsSeen: number;
  ledgerEventsAdded: number;
  sourceEventsDeduped: number;
  invalidEvents: number;
}

async function flushAccumulator(accumulator: ReplayAccumulator): Promise<void> {
  if (!accumulator.buffer.length) return;
  const batch = accumulator.buffer.splice(0, accumulator.buffer.length);
  const result = await accumulator.runtime.knowledgeIngestor.backfillToolUsage(batch);
  accumulator.sourceEventsSeen += result.sourceEventsSeen;
  accumulator.ledgerEventsAdded += result.ledgerEventsAdded;
  accumulator.sourceEventsDeduped += result.sourceEventsDeduped;
  accumulator.invalidEvents += result.invalidEvents;
}

export async function replayKnowledgeRuntimes(
  runtimes: readonly FolderRuntime[],
  usageStore: ToolUsageStoreContract
): Promise<KnowledgeReplayResult> {
  const accumulators = new Map<string, ReplayAccumulator>(runtimes.map(runtime => [runtime.folderId, {
    runtime,
    buffer: [] as ToolUsageJsonObject[],
    sourceEventsSeen: 0,
    ledgerEventsAdded: 0,
    sourceEventsDeduped: 0,
    invalidEvents: 0
  }]));

  const scan = await usageStore.visitDiagnosticRecords(async record => {
    if (record.event_type !== 'tool_call') return;
    const workspaceId = typeof record.selected_workspace_id === 'string' ? record.selected_workspace_id : '';
    const accumulator = accumulators.get(workspaceId);
    if (!accumulator) return;
    accumulator.buffer.push(record);
    if (accumulator.buffer.length >= REPLAY_BATCH_SIZE) await flushAccumulator(accumulator);
  });

  await Promise.all([...accumulators.values()].map(flushAccumulator));
  const workspaces: KnowledgeReplayWorkspaceResult[] = [];
  for (const [workspaceId, accumulator] of accumulators) {
    const materialized = await accumulator.runtime.knowledgeIngestor.finalizeHistoricalReplay({
      sourceRevision: scan.sourceRevision,
      replayComplete: true
    });
    workspaces.push({
      workspaceId,
      sourceEventsSeen: accumulator.sourceEventsSeen,
      ledgerEventsAdded: accumulator.ledgerEventsAdded,
      sourceEventsDeduped: accumulator.sourceEventsDeduped,
      invalidEvents: accumulator.invalidEvents,
      materializedEvents: materialized.materializedEvents,
      materializedRecords: materialized.materializedRecords,
      ignoredEvents: materialized.ignoredEvents
    });
  }
  workspaces.sort((left, right) => left.workspaceId.localeCompare(right.workspaceId));
  return {
    sourceRevision: scan.sourceRevision,
    scannedLines: scan.scannedLines,
    invalidLines: scan.invalidLines,
    duplicateRecordsIgnored: scan.duplicateRecordsIgnored,
    uniqueRecords: scan.uniqueRecords,
    workspaces
  };
}
