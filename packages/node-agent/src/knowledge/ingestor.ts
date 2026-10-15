import type { JsonObject } from '../types.js';
import { createHash } from 'node:crypto';
import { ExperienceCompiler, EXPERIENCE_COMPILER_REVISION } from './compiler.js';
import { ExperienceLedger, experienceLedgerRecordToObservation, type ExperienceLedgerState } from './ledger.js';
import {
  CanaryImpactStore,
  CanaryStrategyEngine,
  evaluateCanaryPromotions,
  evaluateCanaryRollbacks
} from './canary.js';
import {
  evaluateEvolvedSkillPromotions,
  evaluateEvolvedSkillRollbacks
} from './evolvedSkillCanary.js';
import { KnowledgeImpactStore } from './impact.js';
import { ShadowEvaluator } from './shadow.js';
import { KnowledgeStore } from './store.js';
import { ToolEvolutionPlanner } from './toolEvolution.js';
import { ValidationGate } from './validation.js';
import type { ExperienceObservation, TaskOutcome } from './types.js';

const DEFAULT_QUEUE_CAPACITY = 1_024;
const MAX_DRAIN_BATCH = 256;
const TELEMETRY_FEATURES = [
  'calculate_total',
  'files_considered',
  'scanned_files',
  'scan_excluded_worktree_container_count',
  'returned_count',
  'knowledge_shadow_equivalence_implementation',
  'knowledge_shadow_equivalence_passed',
  'phase_baseline_capture_ms',
  'phase_baseline_refresh_ms',
  'phase_harness_begin_ms',
  'phase_harness_finish_ms',
  'baseline_refresh_mode',
  'baseline_refresh_file_count',
  'recovery_succeeded',
  'error_direct_recovery_action_count',
  'graph_action',
  'detached',
  'reattached',
  'graph_yield_ms',
  'graph_wait_ms',
  'parallel_conflict',
  'parallel_serialized',
  'caller_intent_calculate_total_explicit',
  'caller_intent_calculate_total',
  'caller_intent_count_only',
  'caller_intent_patch_bytes',
  'caller_intent_patch_file_count',
  'caller_intent_patch_hunk_count',
  'caller_intent_wait_timeout_ms',
  'caller_intent_wait_until',
  'caller_intent_wait_timeout_explicit',
  'wait_timed_out',
  'process_completed',
  'event_count',
  'error_recommended_tool'
] as const;

export const EXPERIENCE_NORMALIZER_REVISION = createHash('sha256')
  .update(`experience-normalizer-v1\n${TELEMETRY_FEATURES.join(',')}`)
  .digest('hex');

function finiteNumber(value: unknown): number | undefined {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? Math.max(0, parsed) : undefined;
}

function scalar(value: unknown): string | number | boolean | null | undefined {
  if (value === null || ['string', 'number', 'boolean'].includes(typeof value)) return value as string | number | boolean | null;
  return undefined;
}

export interface ExperienceAttribution {
  operationId?: string;
  taskId?: string;
  conversationContextId?: string;
  skill?: { source: string; name: string; contentSha256: string; generation: number };
  canary?: ExperienceObservation['canary'];
  taskOutcome?: TaskOutcome;
}

export function toolUsageRecordToObservation(
  record: JsonObject,
  expectedWorkspaceId?: string,
  attribution: ExperienceAttribution = {}
): ExperienceObservation | undefined {
  if (record.event_type !== 'tool_call' || typeof record.tool !== 'string' || !record.tool) return undefined;
  if (expectedWorkspaceId && record.selected_workspace_id !== expectedWorkspaceId) return undefined;
  const eventId = typeof record.diagnostic_event_id === 'string' ? record.diagnostic_event_id : '';
  if (!eventId) return undefined;
  const features: Record<string, string | number | boolean | null> = {};
  for (const key of TELEMETRY_FEATURES) {
    const value = scalar(record[key]);
    if (value !== undefined) features[key] = value;
  }
  if (record.recovery_attempt === true) features.retry_attempt = true;
  const verificationOutcome = record.verification_ok === true
    ? 'passed'
    : record.verification_ok === false ? 'failed' : undefined;
  const recoveryOutcome = record.recovery_attempt === true
    ? record.recovery_succeeded === true ? 'passed' : 'failed'
    : undefined;
  const recordCanary = typeof record.knowledge_canary_id === 'string'
    && typeof record.knowledge_canary_implementation === 'string'
    && record.knowledge_canary_eligible === true
    ? {
        knowledgeId: record.knowledge_canary_id,
        implementation: record.knowledge_canary_implementation,
        eligible: true,
        applied: record.knowledge_canary_applied === true,
        bucket: finiteNumber(record.knowledge_canary_bucket) ?? 0
      }
    : undefined;
  const canary = recordCanary ?? (record.tool === 'edit' ? attribution.canary : undefined);
  return {
    eventId,
    tool: record.tool,
    outcome: record.outcome === 'success' && record.verification_ok !== false ? 'success' : 'failure',
    durationMs: finiteNumber(record.duration_ms) ?? 0,
    requestBytes: finiteNumber(record.request_json_bytes),
    responseBytes: finiteNumber(record.response_json_bytes),
    ...(typeof record.error_code === 'string' && record.error_code ? { errorCode: record.error_code } : {}),
    ...(Object.keys(features).length ? { features } : {}),
    ...(verificationOutcome ? { verificationOutcome } : {}),
    ...(recoveryOutcome ? { recoveryOutcome } : {}),
    ...(attribution.operationId ? { operationId: attribution.operationId } : {}),
    ...(attribution.taskId ? { taskId: attribution.taskId } : {}),
    ...(attribution.conversationContextId ? { conversationContextId: attribution.conversationContextId } : {}),
    ...(attribution.skill ? { skill: { ...attribution.skill } } : {}),
    ...(typeof record.runtime_boot_id === 'string' && record.runtime_boot_id ? { runtimeBootId: record.runtime_boot_id } : {}),
    ...(attribution.taskOutcome ? { taskOutcome: attribution.taskOutcome } : {}),
    ...(canary ? { canary } : {}),
    timestampMs: finiteNumber(record.completed_ts_ms) ?? finiteNumber(record.timestamp_ms) ?? Date.now()
  };
}

export interface KnowledgeIngestionSnapshot {
  queued: number;
  processed: number;
  matched: number;
  ignored: number;
  failed: number;
  dropped: number;
  shadowConsidered: number;
  shadowMatched: number;
  shadowActivated: number;
  taskCredits: number;
  validationEvaluated: number;
  validationValidated: number;
  validationBlocked: number;
  canaryObserved: number;
  canaryTaskCredits: number;
  canaryEvaluated: number;
  canaryPromoted: number;
  canaryRolledBack: number;
  toolEvolutionProposalsPlanned: number;
  toolEvolutionProposalsSuperseded: number;
  draining: boolean;
  lastError?: string;
}

export class KnowledgeIngestor {
  private readonly compiler = new ExperienceCompiler();
  private readonly queue: ExperienceObservation[] = [];
  private drainPromise: Promise<void> | undefined;
  private processed = 0;
  private matched = 0;
  private ignored = 0;
  private failed = 0;
  private dropped = 0;
  private shadowConsidered = 0;
  private shadowMatched = 0;
  private shadowActivated = 0;
  private taskCredits = 0;
  private validationEvaluated = 0;
  private validationValidated = 0;
  private validationBlocked = 0;
  private canaryObserved = 0;
  private canaryTaskCredits = 0;
  private canaryEvaluated = 0;
  private canaryPromoted = 0;
  private canaryRolledBack = 0;
  private toolEvolutionProposalsPlanned = 0;
  private toolEvolutionProposalsSuperseded = 0;
  private lastError: string | undefined;
  private readonly shadowEvaluator: ShadowEvaluator;
  private readonly validationGate: ValidationGate;
  private readonly ledger: ExperienceLedger;

  constructor(
    private readonly store: KnowledgeStore,
    private readonly impactStore: KnowledgeImpactStore,
    private readonly workspaceId: string,
    private readonly queueCapacity = DEFAULT_QUEUE_CAPACITY,
    private readonly canary?: {
      impactStore: CanaryImpactStore;
      strategyEngine: CanaryStrategyEngine;
    },
    private readonly toolEvolutionPlanner?: ToolEvolutionPlanner
  ) {
    this.shadowEvaluator = new ShadowEvaluator(store, impactStore);
    this.validationGate = new ValidationGate(store, impactStore);
    this.ledger = new ExperienceLedger(store.rootDir, {
      compilerRevision: EXPERIENCE_COMPILER_REVISION,
      normalizerRevision: EXPERIENCE_NORMALIZER_REVISION
    });
  }

  enqueueToolUsage(record: JsonObject, attribution: ExperienceAttribution = {}): void {
    const observation = toolUsageRecordToObservation(record, this.workspaceId, attribution);
    if (!observation) {
      this.ignored += 1;
      return;
    }
    if (this.queue.length >= this.queueCapacity) {
      this.dropped += 1;
      return;
    }
    this.queue.push(observation);
    this.startDrain();
  }

  async backfillToolUsage(records: readonly JsonObject[]): Promise<{
    sourceEventsSeen: number;
    ledgerEventsAdded: number;
    sourceEventsDeduped: number;
    invalidEvents: number;
  }> {
    const observations: ExperienceObservation[] = [];
    let invalidEvents = 0;
    for (const record of records) {
      const observation = toolUsageRecordToObservation(record, this.workspaceId);
      if (!observation) {
        invalidEvents += 1;
        continue;
      }
      observations.push(observation);
    }
    const appended = await this.ledger.append(observations);
    return {
      sourceEventsSeen: records.length,
      ledgerEventsAdded: appended.added,
      sourceEventsDeduped: appended.deduped,
      invalidEvents: invalidEvents + appended.invalid
    };
  }

  async finalizeHistoricalReplay(args: { sourceRevision?: string; replayComplete: boolean }): Promise<{
    materializedEvents: number;
    materializedRecords: number;
    ignoredEvents: number;
  }> {
    const pending = await this.ledger.pendingMaterialization();
    let materializedRecords = 0;
    let ignoredEvents = 0;
    let updatedAtMs = 0;
    for (const ledgerRecord of pending) {
      const observation = experienceLedgerRecordToObservation(ledgerRecord);
      const records = this.compiler.compile([observation]);
      if (!records.length) ignoredEvents += 1;
      for (const record of records) {
        await this.store.upsert(record);
        materializedRecords += 1;
      }
      updatedAtMs = Math.max(updatedAtMs, observation.timestampMs);
    }
    if (pending.length) {
      await this.ledger.advanceMaterialized(ignoredEvents, pending.length);
      this.shadowActivated += await this.shadowEvaluator.activateCandidates(updatedAtMs || Date.now());
      if (this.toolEvolutionPlanner) {
        const proposalSync = await this.toolEvolutionPlanner.sync(updatedAtMs || Date.now());
        this.toolEvolutionProposalsPlanned += proposalSync.planned;
        this.toolEvolutionProposalsSuperseded += proposalSync.superseded;
      }
    }
    await this.ledger.markReplay(args.sourceRevision, args.replayComplete);
    return { materializedEvents: pending.length, materializedRecords, ignoredEvents };
  }

  durableSnapshot(): Promise<ExperienceLedgerState> {
    return this.ledger.snapshot();
  }

  snapshot(): KnowledgeIngestionSnapshot {
    return {
      queued: this.queue.length,
      processed: this.processed,
      matched: this.matched,
      ignored: this.ignored,
      failed: this.failed,
      dropped: this.dropped,
      shadowConsidered: this.shadowConsidered,
      shadowMatched: this.shadowMatched,
      shadowActivated: this.shadowActivated,
      taskCredits: this.taskCredits,
      validationEvaluated: this.validationEvaluated,
      validationValidated: this.validationValidated,
      validationBlocked: this.validationBlocked,
      canaryObserved: this.canaryObserved,
      canaryTaskCredits: this.canaryTaskCredits,
      canaryEvaluated: this.canaryEvaluated,
      canaryPromoted: this.canaryPromoted,
      canaryRolledBack: this.canaryRolledBack,
      toolEvolutionProposalsPlanned: this.toolEvolutionProposalsPlanned,
      toolEvolutionProposalsSuperseded: this.toolEvolutionProposalsSuperseded,
      draining: Boolean(this.drainPromise),
      ...(this.lastError ? { lastError: this.lastError } : {})
    };
  }

  async flush(): Promise<void> {
    while (this.drainPromise || this.queue.length) {
      this.startDrain();
      if (this.drainPromise) await this.drainPromise;
    }
  }

  private startDrain(): void {
    if (this.drainPromise || !this.queue.length) return;
    this.drainPromise = this.drain().finally(() => {
      this.drainPromise = undefined;
      if (this.queue.length) this.startDrain();
    });
  }

  private async drain(): Promise<void> {
    while (this.queue.length) {
      const batch = this.queue.splice(0, MAX_DRAIN_BATCH);
      this.processed += batch.length;
      try {
        const recordsBeforeBatch = await this.store.list();
        const ledgerAppend = await this.ledger.append(batch);
        const shadow = await this.shadowEvaluator.evaluate(batch, recordsBeforeBatch);
        this.shadowConsidered += shadow.considered;
        this.shadowMatched += shadow.matched;
        const canaryObserved = this.canary ? await this.canary.impactStore.recordObservations(batch) : 0;
        this.canaryObserved += canaryObserved;
        let validationRelevant = shadow.matched > 0;
        let canaryRelevant = canaryObserved > 0;
        const records = this.compiler.compile(batch);
        const matchedEvents = new Set(records.flatMap(record => record.sourceEventIds));
        this.matched += matchedEvents.size;
        this.ignored += batch.length - matchedEvents.size;
        for (const record of records) await this.store.upsert(record);
        const updatedAtMs = Math.max(...batch.map(observation => observation.timestampMs));
        this.shadowActivated += await this.shadowEvaluator.activateCandidates(updatedAtMs);
        if (this.toolEvolutionPlanner) {
          const proposalSync = await this.toolEvolutionPlanner.sync(updatedAtMs);
          this.toolEvolutionProposalsPlanned += proposalSync.planned;
          this.toolEvolutionProposalsSuperseded += proposalSync.superseded;
        }
        const terminalTasks = new Map<string, { outcome: TaskOutcome; timestampMs: number }>();
        for (const observation of batch) {
          if (!observation.taskId || !observation.taskOutcome) continue;
          const existing = terminalTasks.get(observation.taskId);
          if (!existing || observation.timestampMs >= existing.timestampMs) {
            terminalTasks.set(observation.taskId, { outcome: observation.taskOutcome, timestampMs: observation.timestampMs });
          }
        }
        for (const [taskId, terminal] of terminalTasks) {
          const credited = await this.impactStore.creditTaskOutcome(taskId, terminal.outcome, terminal.timestampMs);
          this.taskCredits += credited;
          if (credited > 0) validationRelevant = true;
          if (this.canary) {
            const canaryCredits = await this.canary.impactStore.creditTaskOutcome(taskId, terminal.outcome, terminal.timestampMs);
            this.canaryTaskCredits += canaryCredits;
            if (canaryCredits > 0) canaryRelevant = true;
          }
        }
        if (validationRelevant) {
          const validation = await this.validationGate.evaluate(updatedAtMs);
          this.validationEvaluated += validation.evaluated;
          this.validationValidated += validation.validated;
          this.validationBlocked += validation.blocked;
          if (validation.validated > 0 && this.canary) await this.canary.strategyEngine.refresh();
        }
        if (canaryRelevant && this.canary) {
          const toolRollback = await evaluateCanaryRollbacks(this.store, this.impactStore, this.canary.impactStore, updatedAtMs);
          const skillRollback = await evaluateEvolvedSkillRollbacks(this.store, this.canary.impactStore, updatedAtMs);
          const rolledBack = toolRollback.rolledBack + skillRollback.rolledBack;
          this.canaryEvaluated += toolRollback.evaluated + skillRollback.evaluated;
          this.canaryRolledBack += rolledBack;
          const toolPromotion = toolRollback.rolledBack > 0
            ? { evaluated: 0, promoted: 0 }
            : await evaluateCanaryPromotions(this.store, this.canary.impactStore, updatedAtMs);
          const skillPromotion = skillRollback.rolledBack > 0
            ? { evaluated: 0, promoted: 0 }
            : await evaluateEvolvedSkillPromotions(this.store, this.canary.impactStore, updatedAtMs);
          this.canaryPromoted += toolPromotion.promoted + skillPromotion.promoted;
          if (toolRollback.rolledBack > 0 || toolPromotion.promoted > 0) await this.canary.strategyEngine.refresh();
        }
        if (ledgerAppend.added > 0) await this.ledger.advanceMaterialized(0, ledgerAppend.added);
        this.lastError = undefined;
      } catch (error) {
        this.failed += batch.length;
        this.lastError = error instanceof Error ? error.message : String(error);
      }
    }
  }
}
