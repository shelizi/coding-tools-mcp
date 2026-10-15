import { stat } from 'node:fs/promises';
import { toolNamesForProfile, toolsetRevisionForProfile } from '../catalog.js';
import type { ConversationIdentity } from '../conversation/contract.js';
import { ConversationRoutingError } from '../conversation.js';
import { ABSOLUTE_COMMAND_TIMEOUT_MAX_MS } from '../executionLimits.js';
import { ABSOLUTE_JOB_TIMEOUT_MAX_MS, configuredJobTimeoutMaxMs } from '../processes/timeoutPolicy.js';
import { bootstrapHistory } from '../history.js';
import type { ToolDispatchRequest, ToolHandlerMap } from '../toolDispatch/contract.js';
import type { JsonObject, ToolContext } from '../types.js';
import { runtimeRevisionForWorkspace } from '../runtimeRevision.js';
import { skillSummary } from '../skills/types.js';
import { declaredToolStrategyImplementation, toolStrategyImplementationSnapshot } from '../knowledge/strategyImplementations.js';
import { ValidationGate } from '../knowledge/validation.js';
import { AGENT_VERSION, CLIENT_COMPAT_VERSION } from '../version.js';
import { resolveExistingDirectory, resolveInside, rootAndCwd, selectedFolderSafe, validatedFolderCwd } from '../workspace.js';
import { normalizedSandboxConfig, sandboxAvailable, sandboxBackend, sandboxBackends } from '../sandbox.js';
import { LATEST_MCP_PROTOCOL_VERSION, SUPPORTED_MCP_PROTOCOL_VERSIONS } from '../mcpTransport.js';
import { validateWslWorkspacePath } from '../wsl.js';

function ok(value: JsonObject = {}): JsonObject {
  return { ok: true, ...value };
}

function knowledgeStatusFunnel(records: readonly { status: string }[]): JsonObject {
  const statuses = ['observed', 'candidate', 'shadow', 'validated', 'promoted', 'rejected', 'deprecated', 'superseded'] as const;
  const funnel: JsonObject = { total: records.length };
  for (const status of statuses) funnel[status] = records.filter(record => record.status === status).length;
  return funnel;
}

function roundedRate(numerator: number, denominator: number): number {
  return denominator > 0 ? Math.round((numerator / denominator) * 10_000) / 10_000 : 0;
}

function roundedAverage(total: number, count: number): number {
  return count > 0 ? Math.round((total / count) * 100) / 100 : 0;
}

function percentile(values: readonly number[] | undefined, fraction: number): number | null {
  if (!values?.length) return null;
  const sorted = values.filter(Number.isFinite).map(value => Math.max(0, value)).sort((left, right) => left - right);
  if (!sorted.length) return null;
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(sorted.length * fraction) - 1));
  return Math.round(sorted[index]! * 100) / 100;
}

function durationStats(total: number, count: number, samples: readonly number[] | undefined): JsonObject {
  return {
    avg: roundedAverage(total, count),
    p50: percentile(samples, 0.5),
    p95: percentile(samples, 0.95),
    samples: samples?.length ?? 0,
    distribution_available: Boolean(samples?.length)
  };
}

function boundedLearningSnapshot(
  records: readonly any[],
  shadowImpacts: readonly any[],
  canaryImpacts: readonly any[],
  validation: any,
  durable: any
): JsonObject {
  const funnel = knowledgeStatusFunnel(records);
  const shadowById = new Map(shadowImpacts.map(item => [item.knowledgeId, item]));
  const canaryById = new Map(canaryImpacts.map(item => [item.knowledgeId, item]));
  const blockerById = new Map(validation.blockers.map((item: any) => [item.knowledgeId, item]));
  const strategies = records
    .filter(record => record.target === 'tool_strategy')
    .sort((left, right) => right.updatedAtMs - left.updatedAtMs || left.id.localeCompare(right.id))
    .slice(0, 20)
    .map(record => {
      const implementation = declaredToolStrategyImplementation(record);
      const shadow = shadowById.get(record.id);
      const canary = canaryById.get(record.id);
      const matchedContexts = shadow ? Math.max(
        shadow.matchedTaskIds?.length ?? 0,
        shadow.matchedConversationContextIds?.length ?? 0,
        shadow.matchedRuntimeBootContextIds?.length ?? 0
      ) : 0;
      const controlCalls = canary?.controlCalls ?? 0;
      const interventionCalls = canary?.interventionCalls ?? 0;
      return {
        id: record.id,
        target: record.target,
        tool: String(record.trigger?.tool ?? implementation?.tool ?? ''),
        status: record.status,
        implementation: implementation?.id ?? null,
        validation_class: implementation?.validationClass ?? 'task_aware',
        observations: record.evidence.observations,
        independent_contexts: matchedContexts,
        shadow_matches: shadow?.matched ?? 0,
        validation_blocker: blockerById.get(record.id) ?? null,
        control_calls: controlCalls,
        intervention_calls: interventionCalls,
        control_failure_rate: roundedRate(canary?.controlFailures ?? 0, controlCalls),
        intervention_failure_rate: roundedRate(canary?.interventionFailures ?? 0, interventionCalls),
        shadow_failure_rate: roundedRate(shadow?.actualFailures ?? 0, shadow?.matched ?? 0),
        durations_ms: {
          shadow: durationStats(shadow?.totalObservedDurationMs ?? 0, shadow?.matched ?? 0, shadow?.durationSamplesMs),
          control: durationStats(canary?.controlDurationMs ?? 0, controlCalls, canary?.controlDurationSamplesMs),
          intervention: durationStats(canary?.interventionDurationMs ?? 0, interventionCalls, canary?.interventionDurationSamplesMs)
        },
        semantic_mismatch_count: shadow?.semanticEquivalenceFailures ?? 0,
        promoted_at_ms: canary?.promotedAtMs ?? null,
        rollback_at_ms: canary?.rolledBackAtMs ?? null,
        rollback_reason: canary?.rollbackReason ?? null
      };
    });
  return {
    experience: {
      ledger_events: durable.ledgerEvents ?? 0,
      materialized_events: durable.materializedEvents ?? 0,
      replay_complete: durable.replayComplete ?? false,
      compiler_revision: durable.compilerRevision ?? null,
      normalizer_revision: durable.normalizerRevision ?? null
    },
    funnel,
    top_blockers: validation.blockers.slice(0, 10).map((blocker: any) => ({
      knowledge_id: blocker.knowledgeId,
      target: blocker.target,
      reason: blocker.reason,
      actual: blocker.actual,
      ...(blocker.required === undefined ? {} : { required: blocker.required })
    })),
    top_blockers_truncated: Math.max(0, validation.blockers.length - 10) + validation.blockersTruncated,
    strategies,
    strategies_truncated: Math.max(0, records.filter(record => record.target === 'tool_strategy').length - strategies.length)
  };
}

function workspaceHistoryDir(folder: { path: string }): string {
  return resolveInside(folder.path, 'docs/history-session');
}

function historyBootstrapError(error: unknown): JsonObject {
  const record = error && typeof error === 'object' ? error as Record<string, unknown> : {};
  const code = typeof record.code === 'string' && record.code ? record.code : 'HISTORY_BOOTSTRAP_FAILED';
  const message = error instanceof Error && error.message ? error.message : 'Session history bootstrap failed.';
  const category = typeof record.category === 'string' && record.category
    ? record.category
    : ['EACCES', 'EPERM'].includes(code) ? 'permission' : 'history';
  const retryable = typeof record.retryable === 'boolean'
    ? record.retryable
    : ['EACCES', 'EPERM', 'EBUSY'].includes(code);
  const details = record.details && typeof record.details === 'object' && !Array.isArray(record.details)
    ? record.details as JsonObject
    : {};
  return { code, message, category, retryable, details };
}

function workspaceFolderListing(ctx: ToolContext, key: string, identity: ConversationIdentity): JsonObject {
  const routable = identity.source !== 'missing_mcp_conversation';
  const selectedFolderId = routable ? ctx.selections.get(key) : undefined;
  const defaultCwd = selectedFolderId ? validatedFolderCwd(ctx, key, selectedFolderId) : null;
  const selectionScope = selectedFolderId
    ? identity.isolated ? 'conversation' : 'runtime'
    : 'unselected';
  return {
    multi_folder: ctx.config.folders.length > 1,
    profile_id: ctx.workspaceProfileId,
    selected_folder_id: selectedFolderId ?? null,
    selection_scope: selectionScope,
    conversation_isolated: identity.isolated,
    conversation_source: identity.source,
    default_cwd: defaultCwd,
    folders: ctx.config.folders.map(folder => ({
      ...folder,
      selected: folder.id === selectedFolderId,
      history_dir: workspaceHistoryDir(folder),
      default_cwd: routable ? validatedFolderCwd(ctx, key, folder.id) : '.'
    }))
  };
}

async function bindWorkspaceFolder(
  ctx: ToolContext,
  key: string,
  identity: ConversationIdentity,
  id: string
): Promise<JsonObject> {
  if (identity.requiresConversation && !identity.isolated) {
    throw new ConversationRoutingError(
      'WORKSPACE_FOLDER_NOT_SELECTED',
      'MCP conversation/session identity is missing; a workspace folder cannot be bound without isolated conversation metadata.',
      false,
      { selection_scope: 'unselected', conversation_isolated: false }
    );
  }
  const folder = ctx.config.folders.find(item => item.id === id);
  if (!folder) {
    throw new ConversationRoutingError(
      'WORKSPACE_FOLDER_NOT_FOUND',
      `Workspace folder is not allowed: ${id}`,
      false,
      { folder_id: id, available_folder_ids: ctx.config.folders.map(item => item.id) }
    );
  }
  await validateWslWorkspacePath(folder.path);
  const info = await stat(folder.path);
  if (!info.isDirectory()) throw new ConversationRoutingError('WORKSPACE_FOLDER_NOT_DIRECTORY', `Workspace root must be a directory: ${id}`);
  const defaultCwd = ctx.conversations.selectFolder(key, id);
  return {
    selected_folder_id: id,
    selected_folder: folder,
    profile_id: ctx.workspaceProfileId,
    selection_scope: identity.isolated ? 'conversation' : 'runtime',
    conversation_isolated: identity.isolated,
    conversation_source: identity.source,
    history_dir: workspaceHistoryDir(folder),
    default_cwd: defaultCwd,
    resolved_cwd: resolveInside(folder.path, defaultCwd)
  };
}

async function serverInfo({ ctx, key, identity }: ToolDispatchRequest): Promise<JsonObject> {
  const folder = identity.source === 'missing_mcp_conversation' ? undefined : selectedFolderSafe(ctx, key);
  const profileTools = toolNamesForProfile(ctx.config.activeToolProfile);
  const profileRevision = toolsetRevisionForProfile(ctx.config.activeToolProfile);
  const processStartedAtMs = Math.round(Date.now() - process.uptime() * 1000);
  const revision = await runtimeRevisionForWorkspace(folder?.path);
  const sandboxConfig = normalizedSandboxConfig(ctx.config.sandbox);
  const sandboxDescriptor = sandboxBackend(sandboxConfig.backend);
  const sandboxReady = sandboxAvailable(sandboxConfig);
  const sandboxEnforced = sandboxConfig.enabled && sandboxReady;
  const sandboxBoundary = sandboxEnforced
    ? sandboxConfig.backend
    : sandboxConfig.enabled
      ? 'sandbox_unavailable'
      : 'policy_only';
  return ok({
    server: 'coding-tools-mcp-node', title: 'Coding Tools MCP Node Agent', version: AGENT_VERSION,
    client_compat_version: CLIENT_COMPAT_VERSION,
    protocol_version: LATEST_MCP_PROTOCOL_VERSION, supported_protocol_versions: [...SUPPORTED_MCP_PROTOCOL_VERSIONS],
    endpoint_path: '/mcp', auth_enabled: true, auth_type: 'oauth', tool_count: profileTools.length,
    tools: profileTools, toolset_revision: profileRevision, workspace: folder?.path ?? null,
    configured_tool_profile: ctx.config.toolProfile, tool_profile: ctx.config.activeToolProfile,
    runtime_revision: {
      process_started_at_ms: processStartedAtMs,
      ...revision,
      // Kept for older clients. Exact build-SHA matching above is the authoritative trust signal.
      workspace_head_committed_at_ms: null,
      runtime_predates_workspace_head: null
    },
    profile_id: ctx.workspaceProfileId,
    selected_folder_id: folder?.id ?? null,
    selection_scope: folder ? identity.isolated ? 'conversation' : 'runtime' : 'unselected',
    conversation_isolated: identity.isolated,
    conversation_source: identity.source,
    default_cwd: folder ? ctx.conversations.peekCwdFor(key, folder.id) : null,
    permission_mode: ctx.config.permissionMode,
    policy: ctx.config.policy,
    node_version: process.version, platform: process.platform, arch: process.arch,
    environment: {
      filesystem_sandbox: {
        enabled: sandboxConfig.enabled,
        available: sandboxReady,
        enforced: sandboxEnforced,
        backend: sandboxConfig.backend,
        host_supported: sandboxDescriptor?.hostSupported ?? false,
        enforcement_ready: sandboxDescriptor?.enforcementReady ?? false,
        verification_tool: 'exec_health_check',
        live_verification_required: sandboxConfig.enabled,
        backends: sandboxBackends()
      },
      workspace_exec: {
        available: !sandboxConfig.enabled || sandboxReady,
        sandbox_enforced: sandboxEnforced,
        sandbox_backend: sandboxConfig.backend,
        boundary: sandboxBoundary
      }
    },
    limits: {
      ...ctx.config.limits,
      commandTimeoutAbsoluteMaxMs: ABSOLUTE_COMMAND_TIMEOUT_MAX_MS,
      jobTimeoutMaxMs: configuredJobTimeoutMaxMs(),
      jobTimeoutAbsoluteMaxMs: ABSOLUTE_JOB_TIMEOUT_MAX_MS
    },
    tunnel: ctx.tunnelStatus ?? { enabled: false, state: 'disabled', workers: 0, connectedWorkers: 0, completedRequests: 0 },
    native_binary_free: true, unsupported_tunnels: ['frp', 'cloudflare']
  });
}

async function switchWorkspaceFolder({ ctx, key, args, identity }: ToolDispatchRequest): Promise<JsonObject> {
  const id = String(args.folder_id ?? '').trim();
  return ok({
    ...await bindWorkspaceFolder(ctx, key, identity, id),
    next_action: 'Call history_session_bootstrap after selecting a folder for a new conversation.'
  });
}

async function conversationBootstrap({ ctx, key, args, historyArgs, identity }: ToolDispatchRequest): Promise<JsonObject> {
  const listing = workspaceFolderListing(ctx, key, identity);
  const requestedId = String(args.folder_id ?? '').trim();
  const selectedId = typeof listing.selected_folder_id === 'string' ? listing.selected_folder_id : '';
  const id = requestedId || selectedId || (ctx.config.folders.length === 1 ? ctx.config.folders[0]!.id : '');
  if (!id) {
    return ok({
      ...listing,
      needs_folder_selection: true,
      next_action: {
        tool: 'conversation_bootstrap',
        required_arguments: ['folder_id'],
        suggestion: 'Choose one folders entry and retry conversation_bootstrap with its id.'
      }
    });
  }
  const routing = await bindWorkspaceFolder(ctx, key, identity, id);
  const bootstrapArgs = { ...historyArgs };
  delete bootstrapArgs.folder_id;
  const skillRuntime = ctx.folderRuntimes.get(id);
  const skillSnapshot = skillRuntime ? await skillRuntime.skillRegistry.snapshot() : undefined;
  const strategyImplementationSnapshot = toolStrategyImplementationSnapshot();
  const learningSnapshot = skillRuntime ? await Promise.all([
    skillRuntime.knowledgeStore.revision(),
    skillRuntime.toolStrategyRegistry.snapshot(),
    skillRuntime.toolEvolutionRegistry.snapshot(),
    skillRuntime.evolvedSkillRegistry.snapshot(),
    skillRuntime.knowledgeImpactStore.revision(),
    skillRuntime.knowledgeImpactStore.list(),
    skillRuntime.canaryImpactStore.revision(),
    skillRuntime.canaryImpactStore.list(),
    skillRuntime.knowledgeStore.list(),
    new ValidationGate(skillRuntime.knowledgeStore, skillRuntime.knowledgeImpactStore).inspect(),
    skillRuntime.knowledgeIngestor.durableSnapshot()
  ]) : undefined;
  const projectSkills = skillSnapshot ? {
    count: skillSnapshot.skills.length,
    skillset_revision: skillSnapshot.revision,
    skills: skillSnapshot.skills.map(skillSummary),
    diagnostics: skillSnapshot.diagnostics,
    mcp_surfaces: ['prompts/list', 'prompts/get', 'resources/list', 'resources/read'],
    loading_policy: 'Load only clearly relevant workspace or user-level skills; skill guidance never changes runtime permissions.'
  } : { count: 0, skills: [] };
  const runtimeIngestion = skillRuntime?.knowledgeIngestor.snapshot();
  const learningSummary = learningSnapshot ? boundedLearningSnapshot(
    learningSnapshot[8], learningSnapshot[5], learningSnapshot[7], learningSnapshot[9], learningSnapshot[10]
  ) : undefined;
  const learning = learningSnapshot ? {
    knowledge_revision: learningSnapshot[0],
    tool_strategy_revision: learningSnapshot[1].revision,
    tool_strategy_count: learningSnapshot[1].strategies.length,
    tool_strategy_implementation_revision: strategyImplementationSnapshot.revision,
    tool_strategy_implementation_count: strategyImplementationSnapshot.count,
    tool_evolution_revision: learningSnapshot[2].revision,
    tool_evolution_candidate_count: learningSnapshot[2].candidates.length,
    evolved_skill_revision: learningSnapshot[3].revision,
    promoted_evolved_skill_count: learningSnapshot[3].skills.length,
    shadow_impact_revision: learningSnapshot[4],
    shadow_impact_count: learningSnapshot[5].length,
    canary_impact_revision: learningSnapshot[6],
    canary_impact_count: learningSnapshot[7].length,
    canary_promoted_count: learningSnapshot[7].filter(item => Boolean(item.promotedAtMs) && !item.rolledBackAtMs).length,
    canary_rolled_back_count: learningSnapshot[7].filter(item => Boolean(item.rolledBackAtMs)).length,
    validated_tool_strategy_count: learningSnapshot[1].strategies.filter(item => item.status === 'validated').length,
    promoted_tool_strategy_count: learningSnapshot[1].strategies.filter(item => item.status === 'promoted').length,
    validated_tool_evolution_count: learningSnapshot[2].candidates.filter(item => item.status === 'validated').length,
    knowledge_funnel: knowledgeStatusFunnel(learningSnapshot[8]),
    validation_gate: {
      evaluated: learningSnapshot[9].evaluated,
      ready: learningSnapshot[9].ready,
      blocked: learningSnapshot[9].blocked,
      blockers: learningSnapshot[9].blockers.map(blocker => ({
        knowledge_id: blocker.knowledgeId,
        target: blocker.target,
        reason: blocker.reason,
        actual: blocker.actual,
        ...(blocker.required === undefined ? {} : { required: blocker.required })
      })),
      blockers_truncated: learningSnapshot[9].blockersTruncated
    },
    ingestion: runtimeIngestion,
    runtime_ingestion: runtimeIngestion,
    durable_ingestion: learningSnapshot[10],
    durable_lifetime: {
      experience: learningSummary?.experience ?? {},
      knowledge_funnel: learningSummary?.funnel ?? {},
      shadow_impact_count: learningSnapshot[5].length,
      canary_impact_count: learningSnapshot[7].length
    },
    learning_snapshot: learningSummary,
    counter_scopes: {
      ingestion: 'runtime_process',
      runtime_ingestion: 'runtime_process',
      durable_ingestion: 'durable_experience_ledger_and_replay_state',
      durable_lifetime: 'durable_knowledge_impact_and_experience_state',
      learning_snapshot: 'bounded_durable_learning_explanation',
      knowledge_funnel: 'durable_knowledge_store',
      validation_gate: 'durable_knowledge_and_impact_store_snapshot'
    },
    runtime_policy: 'adaptive_rollout_phase_2e',
    canary_policy: 'Validated strategies use a deterministic 25% semantics-preserving canary; evidence-positive strategies promote to 100% rollout, while post-promotion regressions automatically reject and disable them. Caller arguments and static security policy remain authoritative.',
    knowledge_loading_policy: 'Persistent knowledge and rejected/candidate experiments are not injected into inference context; rollout lifecycle and impact remain out of band.'
  } : undefined;
  const legacyStartupFallback = ['list_workspace_folders', 'switch_workspace_folder', 'history_session_bootstrap'];
  let history: JsonObject;
  try {
    history = await bootstrapHistory(ctx, key, bootstrapArgs);
  } catch (error) {
    return ok({
      ...routing,
      needs_folder_selection: false,
      startup_flow: 'workspace_bootstrapped_history_degraded',
      history_bootstrap_ok: false,
      history_bootstrap_error: historyBootstrapError(error),
      warnings: ['Session history could not be initialized; workspace project tools remain available.'],
      recovery_actions: [{
        action: 'retry_history_bootstrap',
        tool: 'history_session_bootstrap',
        suggestion: 'Retry history_session_bootstrap, optionally with a writable history_dir, when durable handoff/checkpointing is needed.'
      }],
      project_skills: projectSkills,
      ...(learning ? { learning } : {}),
      legacy_startup_fallback: legacyStartupFallback
    });
  }
  return {
    ...history,
    ...routing,
    needs_folder_selection: false,
    startup_flow: 'workspace_and_history_bootstrapped',
    history_bootstrap_ok: true,
    project_skills: projectSkills,
    ...(learning ? { learning } : {}),
    legacy_startup_fallback: legacyStartupFallback
  };
}

async function setDefaultCwd({ ctx, key, args }: ToolDispatchRequest): Promise<JsonObject> {
  const resolved = await resolveExistingDirectory(
    rootAndCwd(ctx, key).root,
    String(args.path ?? '.'),
    'Default cwd must be a directory'
  );
  ctx.conversations.setFolderCwd(key, rootAndCwd(ctx, key).folder.id, resolved.display);
  return ok({ default_cwd: resolved.display, resolved_cwd: resolved.full });
}

export const runtimeToolHandlers = {
  server_info: serverInfo,
  list_workspace_folders: ({ ctx, key, identity }) => ok(workspaceFolderListing(ctx, key, identity)),
  switch_workspace_folder: switchWorkspaceFolder,
  conversation_bootstrap: conversationBootstrap,
  query_tool_usage: ({ ctx, args }) => ctx.usageStore.query(args),
  set_default_cwd: setDefaultCwd
} satisfies ToolHandlerMap;
