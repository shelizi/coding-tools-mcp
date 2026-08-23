import { createHash } from 'node:crypto';
import type { JsonObject, ToolContext } from './types.js';
import { toolNamesForProfile, toolsetRevisionForProfile } from './catalog.js';
import { editTool } from './fileTools.js';
import {
  type ProcessRequestLifecycle
} from './processes.js';
import {
  attachHarnessStatus, beginHarnessTracking, finishHarnessTracking,
  HarnessError, type HarnessTracking
} from './taskTools.js';
import { selectedFolderSafe, validatedFolderCwd } from './workspace.js';
import { OutputRedactionContext } from './redaction.js';
import { validateToolPolicy } from './policy.js';
import { currentExecutionBinding, runWithExecutionBinding } from './executionScope.js';
import { currentFolderRuntime, runtimeForFolderId } from './folderRuntime.js';
import { normalizeToolResult, toolErrorResult, toolFail } from './toolContract.js';
import { canCoalesceToolCall, canonicalToolCall, toolRuntimeFor } from './toolRuntime.js';
import { dispatchDomainTool } from './toolDispatch.js';
import { pendingPermissionBinding, permissionDecision } from './permissionTools.js';
import { ConversationRoutingError } from './conversation.js';

const inflightToolCalls = new WeakMap<ToolContext, Map<string, Promise<JsonObject>>>();
const conversationSessionRoutes = new WeakMap<object, Map<string, Map<string, string>>>();

function conversationSessionRouteMap(ctx: ToolContext, key: string): Map<string, string> {
  const routeOwner = ctx.conversations as object;
  let byConversation = conversationSessionRoutes.get(routeOwner);
  if (!byConversation) {
    byConversation = new Map();
    conversationSessionRoutes.set(routeOwner, byConversation);
  }
  let routes = byConversation.get(key);
  if (!routes) {
    if (byConversation.size >= 128) {
      const oldest = byConversation.keys().next().value;
      if (oldest !== undefined) byConversation.delete(oldest);
    }
    routes = new Map();
    byConversation.set(key, routes);
  }
  return routes;
}

function controlSessionId(name: string, args: JsonObject): string | undefined {
  if (['wait_command', 'send_input', 'kill_session'].includes(name)) {
    const value = String(args.session_id ?? '').trim();
    return value || undefined;
  }
  if (name === 'read_output') {
    const outputRef = String(args.output_ref ?? '').trim();
    if (!outputRef.startsWith('output://')) return undefined;
    return outputRef.slice('output://'.length).split('/', 1)[0] || undefined;
  }
  return undefined;
}

function recordConversationSessionRoutes(
  ctx: ToolContext,
  key: string,
  folderId: string | undefined,
  result: JsonObject
): void {
  if (!folderId) return;
  const routes = conversationSessionRouteMap(ctx, key);
  const sessionIds = new Set<string>();
  const collect = (value: unknown): void => {
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    const object = value as JsonObject;
    const sessionId = typeof object.session_id === 'string' ? object.session_id.trim() : '';
    if (sessionId) sessionIds.add(sessionId);
    if (object.output_refs && typeof object.output_refs === 'object' && !Array.isArray(object.output_refs)) {
      for (const outputRef of Object.values(object.output_refs as JsonObject)) {
        if (typeof outputRef !== 'string' || !outputRef.startsWith('output://')) continue;
        const parsed = outputRef.slice('output://'.length).split('/', 1)[0];
        if (parsed) sessionIds.add(parsed);
      }
    }
    if (Array.isArray(object.results)) {
      for (const item of object.results) {
        if (item && typeof item === 'object' && !Array.isArray(item)) {
          collect((item as JsonObject).result ?? item);
        }
      }
    }
  };
  collect(result);
  for (const sessionId of sessionIds) {
    if (routes.size >= 256 && !routes.has(sessionId)) {
      const oldest = routes.keys().next().value;
      if (oldest !== undefined) routes.delete(oldest);
    }
    routes.set(sessionId, folderId);
  }
}

function stableRequestValue(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(stableRequestValue);
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value as Record<string, unknown>)
      .sort(([left], [right]) => left.localeCompare(right))
      .map(([field, nested]) => [field, stableRequestValue(nested)]));
  }
  return value;
}

function inflightCallKey(
  conversationKey: string,
  name: string,
  args: JsonObject,
  binding: ReturnType<typeof executionBindingFor>
): string {
  const material = stableRequestValue({
    conversation_key: conversationKey,
    tool: name,
    arguments: args,
    folder_id: binding.folderId ?? null,
    default_cwd: binding.defaultCwd ?? null,
    requested_workspace_id: binding.requestedWorkspaceId ?? null,
    route_source: binding.routeSource ?? null
  });
  return createHash('sha256').update(JSON.stringify(material)).digest('hex');
}

function contextInflightCalls(ctx: ToolContext): Map<string, Promise<JsonObject>> {
  let calls = inflightToolCalls.get(ctx);
  if (!calls) {
    calls = new Map();
    inflightToolCalls.set(ctx, calls);
  }
  return calls;
}

function elapsedPhaseMs(startedAt: number): number {
  return Math.max(0, Math.round(performance.now() - startedAt));
}

function addPhaseDuration(result: JsonObject, phase: string, durationMs: number): void {
  const phases = result.phase_durations_ms && typeof result.phase_durations_ms === 'object' && !Array.isArray(result.phase_durations_ms)
    ? { ...result.phase_durations_ms as JsonObject }
    : {};
  const previous = Number(phases[phase] ?? 0);
  phases[phase] = Math.max(0, Math.round(Number.isFinite(previous) ? previous : 0)) + Math.max(0, Math.round(durationMs));
  result.phase_durations_ms = phases;
}

const fail = toolFail;

function selectedFolderId(ctx: ToolContext, key: string): string | undefined {
  return selectedFolderSafe(ctx, key)?.id;
}

async function dispatch(
  ctx: ToolContext,
  name: string,
  args: JsonObject,
  meta: unknown,
  processLifecycle?: ProcessRequestLifecycle
): Promise<JsonObject> {
  const identity = ctx.conversations.identity(meta);
  const key = identity.key;
  const historyArgs = identity.isolated
    ? { ...args, _host_session_key: key }
    : identity.source === 'stable_fallback'
      ? { ...args, _fallback_session_key: key }
      : args;
  const domainResult = dispatchDomainTool(name, {
    ctx,
    key,
    identity,
    args,
    historyArgs,
    processLifecycle,
    resumeTool: request => callTool(
      ctx,
      request.name,
      request.args,
      request.meta,
      true,
      processLifecycle,
      { folderId: request.folderId, defaultCwd: request.defaultCwd }
    )
  });
  if (domainResult) return domainResult;

  return fail('UNKNOWN_TOOL', `Unknown tool: ${name}`, 'catalog', true, {
    available_tools: toolNamesForProfile(ctx.config.activeToolProfile),
    toolset_revision: toolsetRevisionForProfile(ctx.config.activeToolProfile)
  });
}

interface ToolBindingOverride {
  folderId: string;
  defaultCwd?: string;
}

function executionBindingFor(
  ctx: ToolContext,
  key: string,
  name: string,
  args: JsonObject,
  override?: ToolBindingOverride,
  requestedWorkspaceId?: string
) {
  const selectedWorkspaceId = ctx.selections.get(key);
  if (override) {
    const runtime = runtimeForFolderId(ctx, override.folderId);
    return {
      ctx, key, folderId: override.folderId,
      defaultCwd: validatedFolderCwd(ctx, key, override.folderId, override.defaultCwd ?? '.'), runtime,
      selectedWorkspaceId, routeSource: 'permission_resume' as const
    };
  }
  if (name === 'list_workspace_folders' || name === 'switch_workspace_folder' || name === 'conversation_bootstrap') return { ctx, key };
  if (requestedWorkspaceId) {
    const runtime = ctx.folderRuntimes.get(requestedWorkspaceId);
    if (!runtime) {
      throw new ConversationRoutingError(
        'WORKSPACE_FOLDER_NOT_FOUND',
        `Workspace folder is not configured: ${requestedWorkspaceId}`,
        false,
        { folder_id: requestedWorkspaceId }
      );
    }
    return {
      ctx, key, folderId: requestedWorkspaceId,
      defaultCwd: validatedFolderCwd(ctx, key, requestedWorkspaceId), runtime,
      requestedWorkspaceId, selectedWorkspaceId, routeSource: 'explicit' as const
    };
  }
  if (name === 'request_permissions') {
    const binding = pendingPermissionBinding(ctx, args);
    if (binding) return { ctx, key, ...binding, selectedWorkspaceId, routeSource: 'resume_id' as const };
  }
  const sessionId = controlSessionId(name, args);
  if (sessionId) {
    const folderId = conversationSessionRouteMap(ctx, key).get(sessionId);
    if (folderId) {
      return {
        ctx, key, folderId, defaultCwd: validatedFolderCwd(ctx, key, folderId),
        runtime: runtimeForFolderId(ctx, folderId), selectedWorkspaceId, routeSource: 'session_id' as const
      };
    }
  }
  if (!selectedWorkspaceId) return { ctx, key };
  return {
    ctx,
    key,
    folderId: selectedWorkspaceId,
    defaultCwd: validatedFolderCwd(ctx, key, selectedWorkspaceId),
    runtime: runtimeForFolderId(ctx, selectedWorkspaceId),
    selectedWorkspaceId,
    routeSource: 'conversation' as const
  };
}

interface RecoveryContext {
  retryOfCallSequence?: number;
  recoveryOfOperationId?: string;
  recoveryActionId?: string;
}

function takeRecoveryContext(args: JsonObject): RecoveryContext | JsonObject {
  const recovery: RecoveryContext = {};
  const retry = args.retry_of_call_sequence;
  delete args.retry_of_call_sequence;
  if (retry !== undefined) {
    if (!Number.isSafeInteger(retry) || Number(retry) <= 0) {
      return fail('INVALID_ARGUMENT', 'retry_of_call_sequence must be a positive integer', 'validation', false);
    }
    recovery.retryOfCallSequence = Number(retry);
  }
  const operationId = args.recovery_of_operation_id;
  delete args.recovery_of_operation_id;
  if (operationId !== undefined) {
    if (typeof operationId !== 'string' || !operationId.trim() || operationId.trim().length > 128) {
      return fail('INVALID_ARGUMENT', 'recovery_of_operation_id must contain 1-128 characters', 'validation', false);
    }
    recovery.recoveryOfOperationId = operationId.trim();
  }
  const actionId = args.recovery_action_id;
  delete args.recovery_action_id;
  if (actionId !== undefined) {
    const token = typeof actionId === 'string' ? actionId.trim() : '';
    if (!token || token.length > 128 || !/^[A-Za-z0-9._:-]+$/.test(token)) {
      return fail('INVALID_ARGUMENT', 'recovery_action_id must be a stable ASCII token', 'validation', false);
    }
    recovery.recoveryActionId = token;
  }
  return recovery;
}

function recoveryRequested(recovery: RecoveryContext): boolean {
  return recovery.retryOfCallSequence !== undefined
    || recovery.recoveryOfOperationId !== undefined
    || recovery.recoveryActionId !== undefined;
}

function failureId(name: string, semanticArgs: JsonObject, result: JsonObject, folderId?: string): string {
  const error = result.error && typeof result.error === 'object' && !Array.isArray(result.error)
    ? result.error as JsonObject
    : {};
  const details = error.details && typeof error.details === 'object' && !Array.isArray(error.details)
    ? error.details as JsonObject
    : {};
  const argumentsSha = createHash('sha256').update(JSON.stringify(semanticArgs)).digest('hex');
  const identity = {
    version: 'tool-failure-v2',
    tool: name,
    arguments_sha256: argumentsSha,
    resolved_workspace_id: folderId ?? null,
    error_code: error.code ?? result.error_code ?? null,
    error_category: error.category ?? result.error_category ?? null,
    stage: details.stage ?? null,
    reason: details.reason ?? null,
    path: details.path ?? null,
    file_index: details.file_index ?? null,
    edit_index: details.edit_index ?? null,
    expected_sha256: details.expected_sha256 ?? null,
    actual_sha256: details.actual_sha256 ?? null
  };
  return createHash('sha256').update(JSON.stringify(identity)).digest('hex');
}

function attachRecoveryMetadata(
  result: JsonObject,
  name: string,
  semanticArgs: JsonObject,
  recovery: RecoveryContext,
  folderId?: string
): JsonObject {
  if (recovery.retryOfCallSequence !== undefined) result.retry_of_call_sequence = recovery.retryOfCallSequence;
  if (recovery.recoveryOfOperationId !== undefined) {
    result.recovery_of_operation_id_hash = createHash('sha256')
      .update(recovery.recoveryOfOperationId)
      .digest('hex');
  }
  if (recovery.recoveryActionId !== undefined) result.recovery_action_id = recovery.recoveryActionId;
  if (recoveryRequested(recovery)) {
    result.recovery_attempt = true;
    result.recovery_succeeded = result.ok === true;
  }
  if (result.ok === false) result.failure_id = failureId(name, semanticArgs, result, folderId);
  return result;
}

function recoveryTelemetryArguments(args: JsonObject, recovery: RecoveryContext): JsonObject {
  return {
    ...args,
    ...(recovery.retryOfCallSequence !== undefined ? { retry_of_call_sequence: recovery.retryOfCallSequence } : {}),
    ...(recovery.recoveryOfOperationId !== undefined ? { recovery_of_operation_id: recovery.recoveryOfOperationId } : {}),
    ...(recovery.recoveryActionId !== undefined ? { recovery_action_id: recovery.recoveryActionId } : {})
  };
}

export async function callTool(
  ctx: ToolContext,
  name: string,
  args: JsonObject,
  meta: unknown,
  skipPermission = false,
  processLifecycle?: ProcessRequestLifecycle,
  bindingOverride?: ToolBindingOverride
): Promise<JsonObject> {
  const key = ctx.conversations.identity(meta).key;
  const cleanArgs = { ...args };
  const parsedRecovery = takeRecoveryContext(cleanArgs);
  if ('ok' in parsedRecovery) return normalizeToolResult(parsedRecovery);
  const recovery = parsedRecovery;
  const rawWorkspaceId = cleanArgs.workspace_folder_id;
  delete cleanArgs.workspace_folder_id;
  const canonical = canonicalToolCall(name, cleanArgs);
  const telemetryArgs = recoveryTelemetryArguments(canonical.args, recovery);
  let requestedWorkspaceId: string | undefined;
  if (rawWorkspaceId !== undefined) {
    if (!toolRuntimeFor(canonical.name).workspaceSelector) {
      return normalizeToolResult(fail(
        'INVALID_ARGUMENT',
        `workspace_folder_id is not supported by ${canonical.name}`,
        'validation',
        false
      ));
    }
    if (typeof rawWorkspaceId !== 'string' || !rawWorkspaceId.trim()) {
      return normalizeToolResult(fail(
        'INVALID_ARGUMENT',
        'workspace_folder_id must be a non-empty string',
        'validation',
        false
      ));
    }
    requestedWorkspaceId = rawWorkspaceId.trim();
  }
  let binding: ReturnType<typeof executionBindingFor>;
  try {
    binding = executionBindingFor(
      ctx,
      key,
      canonical.name,
      canonical.args,
      bindingOverride,
      requestedWorkspaceId
    );
  } catch (error) {
    return normalizeToolResult(toolErrorResult(error));
  }
  if (skipPermission || !canCoalesceToolCall(canonical.name, canonical.args)) {
    return runWithExecutionBinding(
      binding,
      () => callToolInScope(
        ctx,
        canonical.name,
        canonical.args,
        meta,
        skipPermission,
        processLifecycle,
        telemetryArgs,
        recovery
      )
    );
  }

  const callKey = inflightCallKey(key, canonical.name, canonical.args, binding);
  const inflight = contextInflightCalls(ctx);
  const existing = inflight.get(callKey);
  if (existing) {
    const startedAt = Date.now();
    const requestTiming = ctx.usageStore.beginRequest(startedAt);
    const shared = structuredClone(await existing);
    delete shared.phase_durations_ms;
    const durationMs = Date.now() - startedAt;
    Object.assign(shared, {
      coalesced_inflight: true,
      coalesced_wait_ms: durationMs,
      duration_ms: durationMs,
      admission_queue_wait_ms: 0,
      global_admission_wait_ms: 0,
      workspace_admission_wait_ms: 0,
      workspace_lock_wait_ms: 0
    });
    attachRecoveryMetadata(shared, canonical.name, canonical.args, recovery, binding.folderId);
    addPhaseDuration(shared, 'serialization_ms', 0);
    const serializationStartedAt = performance.now();
    const serializedShared = JSON.stringify(shared);
    const serializationMs = elapsedPhaseMs(serializationStartedAt);
    const sharedPhases = shared.phase_durations_ms as JsonObject;
    sharedPhases.serialization_ms = serializationMs;
    const responseBytes = Buffer.byteLength(serializedShared);
    const folderId = binding.folderId ?? selectedFolderId(ctx, key);
    ctx.usageStore.recordToolCall({
      tool: canonical.name,
      arguments: telemetryArgs,
      result: shared,
      startedTsMs: startedAt,
      durationMs,
      requestTiming,
      requestJsonBytes: Buffer.byteLength(JSON.stringify(telemetryArgs)),
      workspaceId: folderId
    });
    ctx.usage.push({ tool: canonical.name, startedAt, durationMs, ok: shared.ok === true, queueWaitMs: 0, lockWaitMs: 0, responseBytes });
    if (ctx.usage.length > 5_000) ctx.usage.splice(0, 1_000);
    return shared;
  }

  const promise = runWithExecutionBinding(
    binding,
    () => callToolInScope(
      ctx,
      canonical.name,
      canonical.args,
      meta,
      skipPermission,
      processLifecycle,
      telemetryArgs,
      recovery
    )
  );
  inflight.set(callKey, promise);
  try {
    return await promise;
  } finally {
    if (inflight.get(callKey) === promise) inflight.delete(callKey);
    if (inflight.size === 0) inflightToolCalls.delete(ctx);
  }
}

async function callToolInScope(
  ctx: ToolContext,
  name: string,
  args: JsonObject,
  meta: unknown,
  skipPermission = false,
  processLifecycle?: ProcessRequestLifecycle,
  telemetryArgs: JsonObject = args,
  recovery: RecoveryContext = {}
): Promise<JsonObject> {
  ctx = { ...ctx, config: structuredClone(ctx.config) };
  const redaction = new OutputRedactionContext(name, args, ctx.config.securityPolicy);
  const startedAt = Date.now();
  const requestTiming = skipPermission ? undefined : ctx.usageStore.beginRequest(startedAt);
  const key = ctx.conversations.identity(meta).key;
  const binding = currentExecutionBinding(ctx, key);
  const workspaceAdmission = binding?.runtime?.admission;
  const lockAdmission = workspaceAdmission ?? ctx.hubAdmission;
  const runtimePolicy = toolRuntimeFor(name);
  const globalLane = runtimePolicy.lane === 'process' ? ctx.hubAdmission.process : runtimePolicy.lane === 'control' ? undefined : ctx.hubAdmission.blocking;
  const workspaceLane = runtimePolicy.lane === 'process' ? workspaceAdmission?.process : runtimePolicy.lane === 'control' ? undefined : workspaceAdmission?.blocking;
  let releaseGlobalLane: (() => void) | undefined;
  let releaseWorkspaceLane: (() => void) | undefined;
  let releaseLocks: (() => void) | undefined;
  let tracking: HarnessTracking | undefined;
  let trackingFinished = false;
  let queueWaitMs = 0;
  let globalAdmissionWaitMs = 0;
  let workspaceAdmissionWaitMs = 0;
  let lockWaitMs = 0;
  let preflightMs = 0;
  let harnessBeginMs = 0;
  let dispatchMs = 0;
  let harnessFinishMs = 0;
  let preflightObserved = false;
  let harnessBeginObserved = false;
  let dispatchObserved = false;
  let result: JsonObject;
  const availableTools = toolNamesForProfile(ctx.config.activeToolProfile);
  const exposedTools = new Set(availableTools);
  const profileRevision = toolsetRevisionForProfile(ctx.config.activeToolProfile);
  const hookCwd = binding?.runtime?.workspacePath ?? process.cwd();
  const hookSessionId = key;
  let hookContext: string[] = [];
  let hookPreBlocked = false;
  let hookBlockResult: JsonObject | undefined;
  if (availableTools.includes(name)) {
    const pre = await ctx.extensions.preToolUse(name, args, hookCwd, hookSessionId, binding?.folderId);
    if (pre.blocked) {
      hookPreBlocked = true;
      hookBlockResult = fail('HOOK_BLOCKED', pre.blocked.message, 'policy', false, { hook_key: pre.blocked.hookKey });
    } else {
      args = pre.input;
      hookContext = pre.context;
    }
  }

  const mapError = (error: unknown): JsonObject => toolErrorResult(error);

  if (!availableTools.includes(name)) {
    result = fail('UNKNOWN_TOOL', `Unknown tool: ${name}`, 'catalog', true, {
      reason: 'unknown_tool',
      suggestion: 'Refresh tools/list and retry with the current tool catalog.',
      tool_profile: ctx.config.activeToolProfile,
      toolset_revision: profileRevision,
      available_tools: availableTools
    });
  } else if (hookBlockResult) {
    result = hookBlockResult;
  } else {
    try {
      await validateToolPolicy(ctx, key, name, args);
      const denied = skipPermission ? undefined : permissionDecision(ctx, key, name, args, meta);
      if (denied) {
        result = denied;
      } else {
        if (globalLane) {
          const globalStarted = Date.now();
          releaseGlobalLane = await globalLane.acquire(30_000, processLifecycle?.signal);
          globalAdmissionWaitMs = Date.now() - globalStarted;
        }
        if (workspaceLane) {
          const workspaceStarted = Date.now();
          releaseWorkspaceLane = await workspaceLane.acquire(30_000, processLifecycle?.signal);
          workspaceAdmissionWaitMs = Date.now() - workspaceStarted;
        }
        queueWaitMs = globalAdmissionWaitMs + workspaceAdmissionWaitMs;
        const groups = runtimePolicy.lockGroups;
        if (groups.length) {
          const lockStarted = Date.now();
          releaseLocks = await lockAdmission.locks.acquire([...groups]);
          lockWaitMs = Date.now() - lockStarted;
        }
        let preflightResult: JsonObject | undefined;
        if (name === 'edit') {
          const preflightStartedAt = performance.now();
          try {
            const checked = await editTool(ctx, key, { ...args, dry_run: true });
            if (args.dry_run === true || checked.ok !== true || checked.status === 'proposal_required') preflightResult = checked;
          } catch (error) {
            preflightResult = mapError(error);
          } finally {
            preflightMs += elapsedPhaseMs(preflightStartedAt);
            preflightObserved = true;
          }
        }
        if (preflightResult) {
          result = preflightResult;
        } else {
          if (!runtimePolicy.harnessTool) {
            const harnessBeginStartedAt = performance.now();
            try {
              tracking = await beginHarnessTracking(ctx, key, name, args);
            } finally {
              harnessBeginMs += elapsedPhaseMs(harnessBeginStartedAt);
              harnessBeginObserved = true;
            }
          }
          const dispatchStartedAt = performance.now();
          try {
            result = await dispatch(ctx, name, args, meta, processLifecycle);
          } catch (error) {
            result = mapError(error);
          } finally {
            dispatchMs += elapsedPhaseMs(dispatchStartedAt);
            dispatchObserved = true;
          }
          if (tracking) {
            const harnessFinishStartedAt = performance.now();
            try {
              result = await finishHarnessTracking(ctx, key, name, args, tracking, result, exposedTools);
              trackingFinished = true;
            } finally {
              harnessFinishMs += elapsedPhaseMs(harnessFinishStartedAt);
            }
          } else if (runtimePolicy.harnessTool && result.ok === false) {
            result = await attachHarnessStatus(ctx, key, result, false, exposedTools);
          }
        }
      }
    } catch (error) {
      result = mapError(error);
      if (tracking && !trackingFinished) {
        const harnessFinishStartedAt = performance.now();
        try {
          result = await finishHarnessTracking(ctx, key, name, args, tracking, result, exposedTools).catch(() => result);
        } finally {
          harnessFinishMs += elapsedPhaseMs(harnessFinishStartedAt);
        }
      } else if (error instanceof HarnessError) {
        result = await attachHarnessStatus(ctx, key, result, false, exposedTools);
      }
    } finally {
      releaseLocks?.();
      releaseWorkspaceLane?.();
      releaseGlobalLane?.();
    }
  }

  if (availableTools.includes(name) && !hookPreBlocked) {
    try {
      const post = await ctx.extensions.postToolUse(
        result.ok === false ? 'PostToolUseFailure' : 'PostToolUse',
        name,
        args,
        result,
        hookCwd,
        hookSessionId,
        binding?.folderId
      );
      if (hookContext.length) result.hook_context = hookContext;
      if (post.feedback.length) result.hook_feedback = post.feedback;
    } catch (error) {
      result.hook_feedback = [`Hook execution failed: ${error instanceof Error ? error.message : String(error)}`];
    }
  }

  const durationMs = Date.now() - startedAt;
  Object.assign(result, {
    execution_lane: runtimePolicy.lane,
    admission_lane: runtimePolicy.lane,
    admission_scope: workspaceLane ? 'global_and_workspace' : globalLane ? 'global' : 'none',
    admission_queue_wait_ms: queueWaitMs,
    global_admission_wait_ms: globalAdmissionWaitMs,
    workspace_admission_wait_ms: workspaceAdmissionWaitMs,
    workspace_lock_wait_ms: lockWaitMs,
    duration_ms: durationMs,
    ...(binding?.folderId ? {
      requested_workspace_id: binding.requestedWorkspaceId ?? null,
      resolved_workspace_id: binding.folderId,
      workspace_route_source: binding.routeSource ?? 'conversation',
      workspace_route_changed: (binding.routeSource ?? 'conversation') !== 'conversation'
        && binding.selectedWorkspaceId !== binding.folderId,
      conversation_selection_changed: false
    } : {})
  });
  attachRecoveryMetadata(result, name, args, recovery, binding?.folderId);
  if (preflightObserved) addPhaseDuration(result, 'preflight_ms', preflightMs);
  if (harnessBeginObserved) addPhaseDuration(result, 'harness_begin_ms', harnessBeginMs);
  if (dispatchObserved) addPhaseDuration(result, 'dispatch_ms', dispatchMs);
  if (harnessFinishMs > 0) addPhaseDuration(result, 'harness_finish_ms', harnessFinishMs);
  result = normalizeToolResult(result);
  result = redaction.redact(result);
  recordConversationSessionRoutes(ctx, key, binding?.folderId, result);
  addPhaseDuration(result, 'serialization_ms', 0);
  const serializationStartedAt = performance.now();
  const serializedResult = JSON.stringify(result);
  const serializationMs = elapsedPhaseMs(serializationStartedAt);
  const resultPhases = result.phase_durations_ms as JsonObject;
  resultPhases.serialization_ms = serializationMs;
  const responseBytes = Buffer.byteLength(serializedResult);
  const folderId = binding?.folderId ?? selectedFolderId(ctx, key);
  if (requestTiming) {
    ctx.usageStore.recordToolCall({
      tool: name,
      arguments: telemetryArgs,
      result,
      startedTsMs: startedAt,
      durationMs,
      requestTiming,
      requestJsonBytes: Buffer.byteLength(JSON.stringify(telemetryArgs)),
      workspaceId: folderId
    });
  }
  ctx.usage.push({ tool: name, startedAt, durationMs, ok: result.ok === true, queueWaitMs, lockWaitMs, responseBytes });
  if (ctx.usage.length > 5_000) ctx.usage.splice(0, 1_000);
  return result;
}
