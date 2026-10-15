import { createHash } from 'node:crypto';
import type { JsonObject } from '../types.js';
import type { KnowledgeRecord } from './types.js';

export type ToolStrategyImplementationId =
  | 'search_exact_total_fast_tail'
  | 'apply_patch_prefer_guarded_edit'
  | 'wait_command_adaptive_poll';
export type ToolStrategyImplementationSafety = 'semantics_preserving' | 'recommendation_only';
export type ToolStrategyValidationClass = 'semantics_preserving' | 'task_aware';

export interface ToolStrategyImplementationSpec {
  id: ToolStrategyImplementationId;
  tool: string;
  safety: ToolStrategyImplementationSafety;
  validationClass: ToolStrategyValidationClass;
  matches(record: KnowledgeRecord, args: JsonObject): boolean;
}

const IMPLEMENTATIONS: readonly ToolStrategyImplementationSpec[] = [
  {
    id: 'search_exact_total_fast_tail',
    tool: 'search_text',
    safety: 'semantics_preserving',
    validationClass: 'semantics_preserving',
    matches(record, args) {
      const declaredImplementation = record.recommendedAction.semantic_preserving_implementation;
      const legacyBoundedSearch = record.recommendedAction.recommendation === 'prefer_bounded_search';
      return record.trigger.tool === 'search_text'
        && (declaredImplementation === 'search_exact_total_fast_tail' || legacyBoundedSearch)
        && args.calculate_total === true
        && args.count_only !== true
        && (typeof args.query === 'string' || (Array.isArray(args.queries) && args.queries.length > 0));
    }
  }
  ,{
    id: 'apply_patch_prefer_guarded_edit',
    tool: 'apply_patch',
    safety: 'recommendation_only',
    validationClass: 'task_aware',
    matches(record, args) {
      if (record.recommendedAction.task_aware_implementation !== 'apply_patch_prefer_guarded_edit'
        || record.recommendedAction.recommendation !== 'prefer_guarded_edit_for_single_file_replacement'
        || typeof args.patch !== 'string') return false;
      const lines = args.patch.replaceAll('\r\n', '\n').split('\n');
      const fileCount = lines.filter(line => /^\+\+\+\s+/.test(line) || /^\*\*\*\s+(?:Update|Add|Delete) File:/.test(line)).length;
      const hunkCount = lines.filter(line => /^@@/.test(line)).length;
      return fileCount === 1 && hunkCount <= 1;
    }
  },
  {
    id: 'wait_command_adaptive_poll',
    tool: 'wait_command',
    safety: 'recommendation_only',
    validationClass: 'task_aware',
    matches(record, args) {
      return record.recommendedAction.task_aware_implementation === 'wait_command_adaptive_poll'
        && record.recommendedAction.recommendation === 'prefer_adaptive_wait'
        && typeof args.session_id === 'string'
        && args.session_id.length > 0;
    }
  }
] as const;

function stableDescriptor(spec: ToolStrategyImplementationSpec) {
  return { id: spec.id, tool: spec.tool, safety: spec.safety, validationClass: spec.validationClass };
}

export function declaredToolStrategyImplementation(record: KnowledgeRecord): ToolStrategyImplementationSpec | undefined {
  if (record.target !== 'tool_strategy') return undefined;
  const declared = record.recommendedAction.semantic_preserving_implementation
    ?? record.recommendedAction.task_aware_implementation;
  if (typeof declared !== 'string') return undefined;
  return IMPLEMENTATIONS.find(spec => declared === spec.id);
}

export function resolveToolStrategyImplementation(
  record: KnowledgeRecord,
  tool: string,
  args: JsonObject
): ToolStrategyImplementationSpec | undefined {
  const legacyBoundedSearch = record.target === 'tool_strategy'
    && record.recommendedAction.recommendation === 'prefer_bounded_search';
  const implementation = declaredToolStrategyImplementation(record)
    ?? (legacyBoundedSearch ? IMPLEMENTATIONS.find(spec => spec.id === 'search_exact_total_fast_tail') : undefined);
  if (!implementation || implementation.tool !== tool) return undefined;
  return implementation.matches(record, args) ? implementation : undefined;
}

export function toolStrategyImplementationSnapshot(): {
  count: number;
  revision: string;
  implementations: Array<{
    id: ToolStrategyImplementationId;
    tool: string;
    safety: ToolStrategyImplementationSafety;
    validationClass: ToolStrategyValidationClass;
  }>;
} {
  const implementations = IMPLEMENTATIONS.map(stableDescriptor);
  const revision = createHash('sha256').update(JSON.stringify(implementations)).digest('hex');
  return { count: implementations.length, revision, implementations };
}
