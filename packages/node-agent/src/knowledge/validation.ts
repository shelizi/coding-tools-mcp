import { KnowledgeImpactStore, type KnowledgeImpactRecord } from './impact.js';
import { KnowledgeStore } from './store.js';
import { declaredToolStrategyImplementation } from './strategyImplementations.js';
import type { KnowledgeArtifactTarget, KnowledgeRecord } from './types.js';

const MIN_SHADOW_MATCHES = 10;
const MIN_SEMANTICS_PRESERVING_SHADOW_MATCHES = 20;
const MIN_SEMANTIC_EQUIVALENCE_CHECKS = 10;
const MIN_SEMANTICS_PRESERVING_CONTEXTS = 2;
const MIN_TASK_OUTCOMES = 3;
const MIN_VERIFIED_TASK_RATE = 0.8;
const MAX_TASK_FAILURE_RATE = 0.2;
const MAX_TOOL_FAILURE_RATE = 0.25;
const TOOL_EVOLUTION_FALLBACK_MATCHES = 20;
const MIN_TOOL_EVOLUTION_AVERAGE_DURATION_MS = 1_000;
const MAX_REPORTED_BLOCKERS = 20;

export type ValidationBlockerReason =
  | 'missing_shadow_impact'
  | 'insufficient_shadow_matches'
  | 'insufficient_task_outcomes'
  | 'insufficient_semantic_equivalence_checks'
  | 'semantic_equivalence_failure'
  | 'verification_failure_observed'
  | 'insufficient_context_diversity'
  | 'verified_task_rate_low'
  | 'task_failure_rate_high'
  | 'tool_failure_rate_high'
  | 'rollback_observed'
  | 'average_duration_below_threshold'
  | 'insufficient_tool_evolution_fallback_matches';

export interface ValidationBlocker {
  knowledgeId: string;
  target: KnowledgeArtifactTarget;
  reason: ValidationBlockerReason;
  actual: number;
  required?: number;
}

export interface ValidationGateSnapshot {
  evaluated: number;
  ready: number;
  blocked: number;
  blockers: ValidationBlocker[];
  blockersTruncated: number;
}

export interface ValidationGateResult extends ValidationGateSnapshot {
  validated: number;
}

interface ValidationAssessment {
  ready: boolean;
  reason?: ValidationBlockerReason;
  actual?: number;
  required?: number;
}

interface RecordAssessment {
  record: KnowledgeRecord;
  assessment: ValidationAssessment;
}

function ratio(numerator: number, denominator: number): number {
  return denominator > 0 ? numerator / denominator : 0;
}

function taskOutcomeCount(impact: KnowledgeImpactRecord): number {
  return impact.taskVerifiedSuccesses
    + impact.taskUnverifiedSuccesses
    + impact.taskFailures
    + impact.taskRollbacks;
}

function blocked(reason: ValidationBlockerReason, actual: number, required?: number): ValidationAssessment {
  return { ready: false, reason, actual, ...(required === undefined ? {} : { required }) };
}

function taskAwareToolStrategyAssessment(impact: KnowledgeImpactRecord): ValidationAssessment {
  const taskOutcomes = taskOutcomeCount(impact);
  if (impact.matched < MIN_SHADOW_MATCHES) return blocked('insufficient_shadow_matches', impact.matched, MIN_SHADOW_MATCHES);
  if (taskOutcomes < MIN_TASK_OUTCOMES) return blocked('insufficient_task_outcomes', taskOutcomes, MIN_TASK_OUTCOMES);
  if (impact.taskRollbacks > 0) return blocked('rollback_observed', impact.taskRollbacks, 0);
  const verifiedTaskRate = ratio(impact.taskVerifiedSuccesses, taskOutcomes);
  if (verifiedTaskRate < MIN_VERIFIED_TASK_RATE) return blocked('verified_task_rate_low', verifiedTaskRate, MIN_VERIFIED_TASK_RATE);
  const taskFailureRate = ratio(impact.taskFailures, taskOutcomes);
  if (taskFailureRate > MAX_TASK_FAILURE_RATE) return blocked('task_failure_rate_high', taskFailureRate, MAX_TASK_FAILURE_RATE);
  const toolFailureRate = ratio(impact.actualFailures, impact.matched);
  if (toolFailureRate > MAX_TOOL_FAILURE_RATE) return blocked('tool_failure_rate_high', toolFailureRate, MAX_TOOL_FAILURE_RATE);
  return { ready: true };
}

function matchedContextDiversity(impact: KnowledgeImpactRecord): number {
  return Math.max(
    impact.matchedTaskIds.length,
    impact.matchedConversationContextIds?.length ?? 0,
    impact.matchedRuntimeBootContextIds?.length ?? 0
  );
}

function semanticsPreservingToolStrategyAssessment(impact: KnowledgeImpactRecord): ValidationAssessment {
  if (impact.matched < MIN_SEMANTICS_PRESERVING_SHADOW_MATCHES) {
    return blocked('insufficient_shadow_matches', impact.matched, MIN_SEMANTICS_PRESERVING_SHADOW_MATCHES);
  }
  const equivalenceChecks = impact.semanticEquivalenceChecks ?? 0;
  if (equivalenceChecks < MIN_SEMANTIC_EQUIVALENCE_CHECKS) {
    return blocked('insufficient_semantic_equivalence_checks', equivalenceChecks, MIN_SEMANTIC_EQUIVALENCE_CHECKS);
  }
  const equivalenceFailures = impact.semanticEquivalenceFailures ?? 0;
  if (equivalenceFailures > 0) return blocked('semantic_equivalence_failure', equivalenceFailures, 0);
  if (impact.verificationFailures > 0) return blocked('verification_failure_observed', impact.verificationFailures, 0);
  if (impact.taskRollbacks > 0) return blocked('rollback_observed', impact.taskRollbacks, 0);
  const contextDiversity = matchedContextDiversity(impact);
  if (contextDiversity < MIN_SEMANTICS_PRESERVING_CONTEXTS) {
    return blocked('insufficient_context_diversity', contextDiversity, MIN_SEMANTICS_PRESERVING_CONTEXTS);
  }
  const toolFailureRate = ratio(impact.actualFailures, impact.matched);
  if (toolFailureRate > MAX_TOOL_FAILURE_RATE) return blocked('tool_failure_rate_high', toolFailureRate, MAX_TOOL_FAILURE_RATE);
  return { ready: true };
}

function toolStrategyAssessment(record: KnowledgeRecord, impact: KnowledgeImpactRecord): ValidationAssessment {
  const implementation = declaredToolStrategyImplementation(record);
  if (implementation?.validationClass === 'semantics_preserving') {
    const semanticAssessment = semanticsPreservingToolStrategyAssessment(impact);
    if (semanticAssessment.ready) return semanticAssessment;
    const taskAssessment = taskAwareToolStrategyAssessment(impact);
    if (taskAssessment.ready) return taskAssessment;
    return semanticAssessment;
  }
  return taskAwareToolStrategyAssessment(impact);
}

function toolEvolutionAssessment(impact: KnowledgeImpactRecord): ValidationAssessment {
  if (impact.matched < MIN_SHADOW_MATCHES) return blocked('insufficient_shadow_matches', impact.matched, MIN_SHADOW_MATCHES);
  const averageDurationMs = ratio(impact.totalObservedDurationMs, impact.matched);
  if (averageDurationMs < MIN_TOOL_EVOLUTION_AVERAGE_DURATION_MS) {
    return blocked('average_duration_below_threshold', averageDurationMs, MIN_TOOL_EVOLUTION_AVERAGE_DURATION_MS);
  }
  const taskOutcomes = taskOutcomeCount(impact);
  if (taskOutcomes < MIN_TASK_OUTCOMES) {
    if (impact.matched >= TOOL_EVOLUTION_FALLBACK_MATCHES) return { ready: true };
    return blocked('insufficient_tool_evolution_fallback_matches', impact.matched, TOOL_EVOLUTION_FALLBACK_MATCHES);
  }
  if (impact.taskRollbacks > 0) return blocked('rollback_observed', impact.taskRollbacks, 0);
  const taskFailureRate = ratio(impact.taskFailures, taskOutcomes);
  if (taskFailureRate > MAX_TASK_FAILURE_RATE) return blocked('task_failure_rate_high', taskFailureRate, MAX_TASK_FAILURE_RATE);
  return { ready: true };
}

function evolvedSkillAssessment(impact: KnowledgeImpactRecord): ValidationAssessment {
  const taskOutcomes = taskOutcomeCount(impact);
  if (impact.matched < MIN_SHADOW_MATCHES) return blocked('insufficient_shadow_matches', impact.matched, MIN_SHADOW_MATCHES);
  if (taskOutcomes < MIN_TASK_OUTCOMES) return blocked('insufficient_task_outcomes', taskOutcomes, MIN_TASK_OUTCOMES);
  if (impact.taskRollbacks > 0) return blocked('rollback_observed', impact.taskRollbacks, 0);
  const verifiedTaskRate = ratio(impact.taskVerifiedSuccesses, taskOutcomes);
  if (verifiedTaskRate < MIN_VERIFIED_TASK_RATE) return blocked('verified_task_rate_low', verifiedTaskRate, MIN_VERIFIED_TASK_RATE);
  const taskFailureRate = ratio(impact.taskFailures, taskOutcomes);
  if (taskFailureRate > MAX_TASK_FAILURE_RATE) return blocked('task_failure_rate_high', taskFailureRate, MAX_TASK_FAILURE_RATE);
  return { ready: true };
}

function validationAssessment(record: KnowledgeRecord, impact: KnowledgeImpactRecord): ValidationAssessment {
  if (record.target === 'tool_strategy') return toolStrategyAssessment(record, impact);
  if (record.target === 'tool_evolution') return toolEvolutionAssessment(impact);
  if (record.target === 'evolved_skill') return evolvedSkillAssessment(impact);
  return { ready: false };
}

function validationTarget(record: KnowledgeRecord): boolean {
  return record.status === 'shadow' && ['tool_strategy', 'tool_evolution', 'evolved_skill'].includes(record.target);
}

function blockerFor(record: KnowledgeRecord, assessment: ValidationAssessment): ValidationBlocker | undefined {
  if (assessment.ready || !assessment.reason) return undefined;
  return {
    knowledgeId: record.id,
    target: record.target,
    reason: assessment.reason,
    actual: assessment.actual ?? 0,
    ...(assessment.required === undefined ? {} : { required: assessment.required })
  };
}

export class ValidationGate {
  constructor(
    private readonly knowledgeStore: KnowledgeStore,
    private readonly impactStore: KnowledgeImpactStore
  ) {}

  async inspect(maxBlockers = MAX_REPORTED_BLOCKERS): Promise<ValidationGateSnapshot> {
    const assessments = await this.assess();
    const blockers = assessments.flatMap(({ record, assessment }) => {
      const item = blockerFor(record, assessment);
      return item ? [item] : [];
    });
    const ready = assessments.filter(item => item.assessment.ready).length;
    const boundedLimit = Math.max(0, maxBlockers);
    return {
      evaluated: assessments.length,
      ready,
      blocked: assessments.length - ready,
      blockers: blockers.slice(0, boundedLimit),
      blockersTruncated: Math.max(0, blockers.length - boundedLimit)
    };
  }

  async evaluate(updatedAtMs: number): Promise<ValidationGateResult> {
    const assessments = await this.assess();
    let validated = 0;
    const blockers: ValidationBlocker[] = [];
    for (const { record, assessment } of assessments) {
      if (!assessment.ready) {
        const item = blockerFor(record, assessment);
        if (item) blockers.push(item);
        continue;
      }
      if (await this.knowledgeStore.transitionStatus(record.id, ['shadow'], 'validated', updatedAtMs)) validated += 1;
    }
    const ready = assessments.filter(item => item.assessment.ready).length;
    return {
      evaluated: assessments.length,
      ready,
      validated,
      blocked: assessments.length - ready,
      blockers: blockers.slice(0, MAX_REPORTED_BLOCKERS),
      blockersTruncated: Math.max(0, blockers.length - MAX_REPORTED_BLOCKERS)
    };
  }

  private async assess(): Promise<RecordAssessment[]> {
    const impacts = new Map((await this.impactStore.list()).map(impact => [impact.knowledgeId, impact]));
    const assessments: RecordAssessment[] = [];
    for (const record of await this.knowledgeStore.list()) {
      if (!validationTarget(record)) continue;
      const impact = impacts.get(record.id);
      assessments.push({
        record,
        assessment: impact
          ? validationAssessment(record, impact)
          : blocked('missing_shadow_impact', 0, 1)
      });
    }
    return assessments;
  }
}
