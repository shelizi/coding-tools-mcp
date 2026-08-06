import { randomUUID } from 'node:crypto';
import { stat } from 'node:fs/promises';
import type { FolderRuntime, JsonObject, PendingOperation, ToolContext } from './types.js';
import { readOnlyTools, toolNamesForProfile, toolsetRevisionForProfile } from './catalog.js';
import {
  applyPatchTool, editFileTool, editManyTool, editTool,
  listFilesTool, patchCheckTool, projectMapTool, readFileTool, readManyTool,
  searchTextTool, viewImageTool
} from './fileTools.js';
import { fileOpsTool } from './fileOpsTools.js';
import { formatFilesTool } from './formatterTools.js';
import {
  gitBlameTool, gitBranchTool, gitCommitTool, gitDiffTool, gitLogTool,
  gitRestoreTool, gitShowTool, gitStageTool, gitStatusTool
} from './gitTools.js';
import { bootstrapHistory, checkpointHistory, validateHistory } from './history.js';
import {
  FINALIZED_SESSION_RETENTION_MS, findProcessOperation, killProcessTree,
  ProcessRequestLifecycle, ProcessToolError, processResult, processStatus,
  pruneProcessSessions, readSessionOutput, removeProcessSession, requireProcessSession,
  runCommandGraph, startAndYield, touchSessionAttachment, waitForSession
} from './processes.js';
import {
  attachHarnessStatus, beginHarnessTracking, changeSummary, finishHarnessTracking, finishTask,
  HarnessError, type HarnessTracking, harnessStatus, listTaskEvents, operationLog, projectState,
  setTaskStatus, startTask, taskContext, updateTask
} from './taskTools.js';
import {
  relativeInside, resolveExistingDirectory, resolveInside, rootAndCwd,
  selectedFolderSafe
} from './workspace.js';
import { AGENT_VERSION, CLIENT_COMPAT_VERSION } from './version.js';
import { OutputRedactionContext } from './redaction.js';
import { validateToolPolicy } from './policy.js';
import { parseWslUncPath, validateWslWorkspacePath } from './wsl.js';
import { currentExecutionBinding, runWithExecutionBinding } from './executionScope.js';
import { currentFolderRuntime, findPendingOperation, runtimeForFolderId } from './folderRuntime.js';
import { type ConversationIdentity, ConversationRoutingError } from './conversation.js';
import { normalizeToolResult, toolErrorResult, toolFail } from './toolContract.js';

const PROCESS_TOOLS = new Set(['exec_command', 'exec_many']);
const CONTROL_TOOLS = new Set(['wait_command', 'resolve_operation', 'list_sessions', 'send_input', 'kill_session', 'read_output', 'request_permissions']);
const HARNESS_TOOLS = new Set([
  'harness_status', 'operation_log', 'project_state', 'start_task', 'update_task',
  'pause_task', 'resume_task', 'finish_task', 'task_context', 'list_task_events', 'change_summary'
]);
const GUARDED_TOOLS = new Set([
  'apply_patch', 'edit', 'edit_file', 'edit_many', 'file_ops', 'format_files',
  'exec_command', 'exec_many', 'kill_session',
  'git_branch', 'git_stage', 'git_commit', 'git_restore'
]);

function ok(value: JsonObject = {}): JsonObject { return { ok: true, ...value }; }
const fail = toolFail;

function lockGroups(name: string): string[] {
  if (name.startsWith('history_session_')) return ['history'];
  if (['apply_patch', 'edit', 'edit_file', 'edit_many', 'file_ops', 'format_files'].includes(name)) return ['workspace_content'];
  if (name === 'git_restore') return ['git', 'workspace_content'];
  if (['git_branch', 'git_stage', 'git_commit'].includes(name)) return ['git'];
  if (['start_task', 'update_task', 'pause_task', 'resume_task', 'finish_task'].includes(name)) return ['task'];
  if (name === 'set_default_cwd') return ['cwd'];
  return [];
}

function permissionKind(name: string): string {
  if (name.startsWith('git_') || name === 'apply_patch' || name === 'edit' || name.startsWith('edit_') || name === 'file_ops') return 'workspace_mutation';
  if (name.startsWith('exec_') || name === 'kill_session') return 'process_execution';
  return 'privileged_operation';
}

const MAX_PENDING_OPERATIONS = 256;

function prunePending(runtime: FolderRuntime, now = Date.now()): void {
  for (const [id, pending] of runtime.pendingOperations) {
    if (pending.expiresAt < now) runtime.pendingOperations.delete(id);
  }
}

function preparePendingInsert(runtime: FolderRuntime): void {
  prunePending(runtime);
  while (runtime.pendingOperations.size >= MAX_PENDING_OPERATIONS) {
    const oldest = [...runtime.pendingOperations.values()]
      .sort((left, right) => left.createdAt - right.createdAt)[0];
    if (!oldest) break;
    runtime.pendingOperations.delete(oldest.resumeId);
  }
}

function permissionDecision(ctx: ToolContext, key: string, name: string, args: JsonObject, meta: unknown): JsonObject | undefined {
  if (name === 'request_permissions' || name === 'list_workspace_folders' || name === 'switch_workspace_folder') return undefined;
  if (ctx.config.permissionMode === 'read-only' && !readOnlyTools.has(name)) {
    return fail('PERMISSION_MODE_READ_ONLY', `${name} is unavailable in read-only mode`, 'policy', false, { permission_mode: 'read-only' });
  }
  if (ctx.config.permissionMode !== 'guarded' || !GUARDED_TOOLS.has(name) || args.confirm === true) return undefined;
  const runtime = currentFolderRuntime(ctx, key);
  const { folder, root, cwd } = rootAndCwd(ctx, key);
  preparePendingInsert(runtime);
  const resumeId = randomUUID();
  const permission = permissionKind(name);
  const pending: PendingOperation = {
    resumeId,
    name,
    args: structuredClone(args),
    meta,
    permission,
    reason: `${name} requires approval in guarded mode`,
    folderId: folder.id,
    workspacePath: folder.path,
    defaultCwd: relativeInside(root, cwd).replaceAll('\\', '/'),
    createdAt: Date.now(),
    expiresAt: Date.now() + 300_000
  };
  runtime.pendingOperations.set(resumeId, pending);
  return fail('PERMISSION_REQUIRED', pending.reason, 'policy', false, {
    permission_request: {
      resume_id: resumeId,
      tool_name: name,
      permission,
      reason: pending.reason,
      workspace_folder_id: folder.id,
      ttl_seconds: 300,
      resume_with: 'request_permissions'
    }
  });
}

function selectedFolderId(ctx: ToolContext, key: string): string | undefined {
  return selectedFolderSafe(ctx, key)?.id;
}

function workspaceHistoryDir(folder: { path: string }): string {
  return resolveInside(folder.path, 'docs/history-session');
}

function workspaceFolderListing(ctx: ToolContext, key: string, identity: ConversationIdentity): JsonObject {
  const routable = identity.source !== 'missing_mcp_conversation';
  const selectedFolderId = routable ? ctx.selections.get(key) : undefined;
  const defaultCwd = selectedFolderId ? ctx.conversations.peekCwdFor(key, selectedFolderId) : null;
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
      default_cwd: routable ? ctx.conversations.peekCwdFor(key, folder.id) : '.'
    }))
  };
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

  switch (name) {
    case 'harness_status': return harnessStatus(ctx, key);
    case 'operation_log': return operationLog(ctx, key, args);
    case 'server_info': {
      const folder = identity.source === 'missing_mcp_conversation' ? undefined : selectedFolderSafe(ctx, key);
      const profileTools = toolNamesForProfile(ctx.config.activeToolProfile);
      const profileRevision = toolsetRevisionForProfile(ctx.config.activeToolProfile);
      return ok({
        server: 'coding-tools-mcp-node', title: 'Coding Tools MCP Node Agent', version: AGENT_VERSION,
        client_compat_version: CLIENT_COMPAT_VERSION,
        protocol_version: '2025-11-25', supported_protocol_versions: ['2025-11-25', '2025-06-18', '2025-03-26'],
        endpoint_path: '/mcp', auth_enabled: true, auth_type: 'oauth', tool_count: profileTools.length,
        tools: profileTools, toolset_revision: profileRevision, workspace: folder?.path ?? null,
        configured_tool_profile: ctx.config.toolProfile, tool_profile: ctx.config.activeToolProfile,
        profile_id: ctx.workspaceProfileId,
        selected_folder_id: folder?.id ?? null,
        selection_scope: folder ? identity.isolated ? 'conversation' : 'runtime' : 'unselected',
        conversation_isolated: identity.isolated,
        conversation_source: identity.source,
        default_cwd: folder ? ctx.conversations.peekCwdFor(key, folder.id) : null,
        permission_mode: ctx.config.permissionMode,
        policy: ctx.config.policy,
        node_version: process.version, platform: process.platform, arch: process.arch,
        limits: ctx.config.limits, tunnel: ctx.tunnelStatus ?? { enabled: false, state: 'disabled', workers: 0, connectedWorkers: 0, completedRequests: 0 },
        native_binary_free: true, unsupported_tunnels: ['frp', 'cloudflare']
      });
    }
    case 'list_workspace_folders': {
      return ok(workspaceFolderListing(ctx, key, identity));
    }
    case 'switch_workspace_folder': {
      if (identity.requiresConversation && !identity.isolated) {
        throw new ConversationRoutingError(
          'WORKSPACE_FOLDER_NOT_SELECTED',
          'MCP conversation/session identity is missing; a workspace folder cannot be bound without isolated conversation metadata.',
          false,
          { selection_scope: 'unselected', conversation_isolated: false }
        );
      }
      const id = String(args.folder_id ?? '').trim();
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
      return ok({
        selected_folder_id: id,
        selected_folder: folder,
        profile_id: ctx.workspaceProfileId,
        selection_scope: identity.isolated ? 'conversation' : 'runtime',
        conversation_isolated: identity.isolated,
        conversation_source: identity.source,
        history_dir: workspaceHistoryDir(folder),
        default_cwd: defaultCwd,
        resolved_cwd: resolveInside(folder.path, defaultCwd),
        next_action: 'Call history_session_bootstrap after selecting a folder for a new conversation.'
      });
    }
    case 'query_tool_usage': return ctx.usageStore.query(args);
    case 'history_session_bootstrap': return bootstrapHistory(ctx, key, args);
    case 'history_session_checkpoint': return checkpointHistory(ctx, key, args);
    case 'history_session_validate': return validateHistory(ctx, key, args);
    case 'project_state': return projectState(ctx, key, args);
    case 'start_task': return startTask(ctx, key, args);
    case 'update_task': return updateTask(ctx, key, args);
    case 'pause_task': return setTaskStatus(ctx, key, 'paused', args);
    case 'resume_task': return setTaskStatus(ctx, key, 'active', args);
    case 'finish_task': return finishTask(ctx, key, args);
    case 'task_context': return taskContext(ctx, key, args);
    case 'list_task_events': return listTaskEvents(ctx, key, args);
    case 'change_summary': return changeSummary(ctx, key, args);
    case 'exec_health_check': {
      const startedAt = Date.now();
      const { root } = rootAndCwd(ctx, key);
      const wsl = parseWslUncPath(root);
      const probeArgs: JsonObject = wsl
        ? {
            script: 'printf exec-health; printf exec-health-stderr >&2',
            shell: 'sh',
            confirm: true,
            timeout_ms: 5_000,
            yield_time_ms: 5_000,
            output_mode: 'tail',
            max_output_bytes: 16_384
          }
        : {
            program: process.platform === 'win32' ? 'node.exe' : 'node',
            args: ['-e', 'process.stdout.write("exec-health"); process.stderr.write("exec-health-stderr")'],
            timeout_ms: 5_000,
            yield_time_ms: 5_000,
            output_mode: 'tail',
            max_output_bytes: 16_384
          };
      try {
        let probe = await startAndYield(ctx, key, probeArgs);
        const sessionCreate = typeof probe.session_id === 'string' && probe.session_id.length > 0;
        if (sessionCreate) {
          const session = requireProcessSession(currentFolderRuntime(ctx, key), probe.session_id, false);
          while (!session.finalizedAt) await waitForSession(session, session.sequence, 5_000, 'finalized');
          probe = processResult(session, { output_mode: 'tail', max_output_bytes: 16_384 });
        }
        const commandRun = Number(probe.process_exit_code ?? probe.exit_code) === 0;
        const stdoutCapture = String(probe.stdout ?? '').includes('exec-health');
        const stderrCapture = String(probe.stderr ?? '').includes('exec-health-stderr');
        const healthy = sessionCreate && commandRun && stdoutCapture && stderrCapture;
        return ok({
          worker: { alive: true },
          session_create: sessionCreate,
          command_run: commandRun,
          stdout_capture: stdoutCapture,
          stderr_capture: stderrCapture,
          duration_ms: Date.now() - startedAt,
          next_actions: healthy ? [] : ['检查 exec worker 日志', '重启运行时'],
          status: healthy ? 'success' : 'error',
          summary: healthy
            ? 'exec worker、session、命令执行和 stdout/stderr 捕获均正常'
            : 'exec health check 未通过，请查看 probe 结果',
          probe
        });
      } catch (error) {
        const normalized = toolErrorResult(error);
        return ok({
          worker: { alive: true },
          session_create: false,
          command_run: false,
          stdout_capture: false,
          stderr_capture: false,
          duration_ms: Date.now() - startedAt,
          next_actions: ['检查 exec worker 日志', '重启运行时'],
          status: 'error',
          summary: 'exec session 创建或探针执行失败',
          error: normalized.error ?? normalized
        });
      }
    }
    case 'set_default_cwd': {
      const resolved = await resolveExistingDirectory(
        rootAndCwd(ctx, key).root,
        String(args.path ?? '.'),
        'Default cwd must be a directory'
      );
      ctx.defaultCwds.set(key, resolved.display);
      return ok({ default_cwd: resolved.display, resolved_cwd: resolved.full });
    }
    case 'read_file': return readFileTool(ctx, key, args);
    case 'read_many': return readManyTool(ctx, key, args);
    case 'project_map': return projectMapTool(ctx, key, args);
    case 'list_files': return listFilesTool(ctx, key, args);
    case 'search_text': return searchTextTool(ctx, key, args);
    case 'apply_patch': return applyPatchTool(ctx, key, args);
    case 'edit': return editTool(ctx, key, args);
    case 'edit_file': return editFileTool(ctx, key, args);
    case 'edit_many': return editManyTool(ctx, key, args);
    case 'file_ops': return fileOpsTool(ctx, key, args);
    case 'format_files': return formatFilesTool(ctx, key, args);
    case 'patch_check': return patchCheckTool(ctx, key, args);
    case 'exec_command': return startAndYield(ctx, key, args, processLifecycle);
    case 'exec_many': return runCommandGraph(ctx, key, args, processLifecycle?.signal);
    case 'wait_command': {
      const runtime = currentFolderRuntime(ctx, key);
      const registryStarted = Date.now();
      const session = requireProcessSession(runtime, args.session_id);
      const sessionRegistryWaitMs = Date.now() - registryStarted;
      const cursor = Math.max(0, Number(args.cursor ?? 0));
      const waitTimeoutMs = Math.max(0, Math.min(120_000, Number(args.timeout_ms ?? 30_000)));
      const heartbeatMs = Math.max(0, Math.min(30_000, Number(args.heartbeat_ms ?? 0)));
      const waitUntil = String(args.until ?? 'output_or_exit');
      const waited = await waitForSession(session, cursor, waitTimeoutMs, waitUntil, heartbeatMs);
      const snapshotStarted = Date.now();
      const requestTimedOut = !waited.changed && !waited.heartbeat;
      const result = processResult(session, { ...args, request_timed_out: requestTimedOut });
      Object.assign(result, {
        session_registry_wait_ms: sessionRegistryWaitMs,
        actual_wait_ms: waited.actualWaitMs,
        snapshot_ms: Date.now() - snapshotStarted,
        heartbeat: waited.heartbeat,
        request_timed_out: requestTimedOut,
        wait_timeout_ms: waitTimeoutMs,
        effective_wait_ms: waited.effectiveWaitMs,
        heartbeat_ms: heartbeatMs,
        wait_until: waitUntil
      });
      if (session.finalizedAt === undefined) {
        result.next_actions = [{
          tool: 'wait_command',
          arguments: {
            session_id: session.id,
            cursor: result.latest_cursor,
            timeout_ms: waitTimeoutMs || 30_000,
            heartbeat_ms: heartbeatMs || 10_000,
            until: waitUntil,
            output_mode: 'delta'
          }
        }];
      }
      if (waited.heartbeat) result.suggestion = 'Heartbeat emitted; continue waiting with next_actions.';
      else if (!session.finalizedAt) result.suggestion = '使用 next_actions 继续等待新输出或进程结束';
      else if (result.termination_reason === 'exited') result.suggestion = '进程已结束；检查 process_exit_code 与 post_checks';
      return result;
    }
    case 'resolve_operation': {
      const operationId = String(args.operation_id ?? '').trim();
      const requestedFingerprint = String(args.command_fingerprint ?? '').trim();
      if (!operationId && !requestedFingerprint) {
        throw new ProcessToolError('INVALID_ARGUMENT', 'operation_id or command_fingerprint is required', 'invalid_argument', false);
      }
      const resolved = findProcessOperation(currentFolderRuntime(ctx, key), operationId, requestedFingerprint);
      if (!resolved.session) {
        throw new ProcessToolError('OPERATION_NOT_FOUND', 'retained command operation was not found', 'not_found', false, {
          operation_id: operationId || null,
          command_fingerprint: requestedFingerprint || null,
          retention_seconds: FINALIZED_SESSION_RETENTION_MS / 1000,
          suggestion: 'Use list_sessions to inspect retained command sessions.'
        });
      }
      touchSessionAttachment(resolved.session);
      return {
        ...processResult(resolved.session, {
          ...args,
          deduplicated: true,
          attached_to_session_id: resolved.session.id
        }),
        resolved_by: resolved.resolvedBy
      };
    }
    case 'list_sessions': {
      const runtime = currentFolderRuntime(ctx, key);
      pruneProcessSessions(runtime);
      const includeFinalized = args.include_finalized !== false;
      const status = args.status === undefined ? undefined : String(args.status);
      const limit = Math.max(1, Math.min(1000, Number(args.limit ?? 100)));
      const all = [...runtime.sessions.values()].sort((left, right) => right.startedAt - left.startedAt);
      const sessions = all
        .filter(session => includeFinalized || !session.finalizedAt)
        .filter(session => !status || processStatus(session) === status)
        .slice(0, limit)
        .map(session => processResult(session, { output_mode: 'none' }));
      return ok({
        sessions,
        count: sessions.length,
        include_finalized: includeFinalized,
        retention_seconds: FINALIZED_SESSION_RETENTION_MS / 1000,
        active_count: all.filter(session => !session.endedAt).length,
        verifying_count: all.filter(session => processStatus(session) === 'verifying').length,
        finalized_count: all.filter(session => Boolean(session.finalizedAt)).length
      });
    }
    case 'send_input': {
      const session = requireProcessSession(currentFolderRuntime(ctx, key), args.session_id);
      if (!session.child || session.endedAt || !session.stdinOpen) {
        throw new ProcessToolError('SESSION_CLOSED', 'Session stdin is closed.', 'runtime', false, {
          session_id: session.id,
          status: processStatus(session)
        });
      }
      const chars = String(args.chars ?? '');
      if (chars) session.child.stdin.write(chars);
      const closeStdin = args.close_stdin === true;
      if (closeStdin) {
        session.child.stdin.end();
        session.stdinOpen = false;
        session.events.emit('change');
      }
      return {
        ...processResult(session, { output_mode: 'none' }),
        bytes_written: Buffer.byteLength(chars),
        stdin_closed: closeStdin,
        suggestion: closeStdin ? 'stdin 已关闭；使用 wait_command 等待进程结束' : 'stdin 已写入；继续使用 send_input 或 wait_command'
      };
    }
    case 'kill_session': {
      const runtime = currentFolderRuntime(ctx, key);
      const session = requireProcessSession(runtime, args.session_id);
      const waitMs = Math.max(0, Math.min(30_000, Number(args.wait_ms ?? 5000)));
      if (!session.endedAt) {
        await killProcessTree(session, String(args.signal ?? 'TERM') as 'TERM' | 'KILL' | 'INT', 'killed');
        if (waitMs > 0) await waitForSession(session, session.sequence, waitMs, 'exit');
      }
      const terminated = Boolean(session.endedAt);
      const result = processResult(session, {
        output_mode: 'tail',
        max_output_bytes: args.max_output_bytes === undefined ? undefined : Number(args.max_output_bytes),
        tail_lines: 100
      });
      Object.assign(result, {
        killed: terminated,
        status: terminated ? 'killed' : 'terminating',
        evicted: terminated
      });
      if (terminated) removeProcessSession(runtime, session.id);
      else {
        result.warnings = [...(Array.isArray(result.warnings) ? result.warnings : []), 'process termination is still pending'];
        result.suggestion = '继续使用 wait_command 确认进程已终止';
      }
      return result;
    }
    case 'read_output': {
      const runtime = currentFolderRuntime(ctx, key);
      pruneProcessSessions(runtime);
      const reference = String(args.output_ref ?? '');
      const match = /^output:\/\/([^/]+)\/(stdout|stderr)$/.exec(reference);
      if (!match) throw new ProcessToolError('INVALID_OUTPUT_REF', 'Invalid output_ref.', 'invalid_argument', false, { output_ref: reference });
      const session = runtime.sessions.get(match[1]);
      if (!session) throw new ProcessToolError('SESSION_NOT_FOUND', `Session not found: ${match[1]}`, 'not_found', false);
      touchSessionAttachment(session);
      const offset = Math.max(0, Number(args.offset ?? 0));
      const limit = Math.max(1, Math.min(1_048_576, Number(args.limit ?? 4096)));
      return readSessionOutput(session, match[2] as 'stdout' | 'stderr', offset, limit);
    }
    case 'git_status': return gitStatusTool(ctx, key, args);
    case 'git_diff': return gitDiffTool(ctx, key, args);
    case 'git_log': return gitLogTool(ctx, key, args);
    case 'git_show': return gitShowTool(ctx, key, args);
    case 'git_blame': return gitBlameTool(ctx, key, args);
    case 'git_branch': return gitBranchTool(ctx, key, args);
    case 'git_stage': return gitStageTool(ctx, key, args);
    case 'git_commit': return gitCommitTool(ctx, key, args);
    case 'git_restore': return gitRestoreTool(ctx, key, args);
    case 'request_permissions': {
      const id = String(args.resume_id ?? '').trim();
      if (!id) {
        if (ctx.config.permissionMode === 'dangerous') {
          return ok({
            status: 'granted',
            grant_id: 'dangerously-skip-all-permissions',
            expires_at: null,
            constraints: {
              mode: 'dangerous',
              workspace: selectedFolderSafe(ctx, key)?.path ?? null,
              requested: args
            },
            warnings: ['dangerous permission mode is enabled; permission-gated operations are auto-granted']
          });
        }
        return {
          ...ok({ status: 'unsupported', grant_id: null, expires_at: null, next_actions: [] }),
          ok: false,
          error: {
            code: 'RESUME_ID_REQUIRED',
            message: 'Provide the resume_id returned by the blocked operation.',
            category: 'permission',
            retryable: true,
            details: { requested: args }
          }
        };
      }
      const found = findPendingOperation(ctx, id);
      if (!found) {
        return fail(
          'RESUME_OPERATION_NOT_FOUND',
          'resume operation was not found or expired',
          'permission',
          false,
          { resume_id: id }
        );
      }
      prunePending(found.runtime);
      const pending = found.runtime.pendingOperations.get(id);
      if (!pending) {
        return fail(
          'RESUME_OPERATION_NOT_FOUND',
          'resume operation was not found or expired',
          'permission',
          false,
          { resume_id: id }
        );
      }
      if (pending.folderId !== found.runtime.folderId || pending.workspacePath !== found.runtime.workspacePath) {
        found.runtime.pendingOperations.delete(id);
        return fail(
          'RESUME_OPERATION_STALE',
          'resume operation no longer matches its original workspace',
          'conflict',
          false,
          { resume_id: id, workspace_folder_id: pending.folderId }
        );
      }
      const approved = ctx.config.permissionMode === 'dangerous' || (args.approve === true && args.confirm === true);
      if (!approved) {
        return fail('PERMISSION_NOT_APPROVED', 'The pending operation was not approved.', 'permission', true, {
          resume_id: id,
          workspace_folder_id: pending.folderId,
          suggestion: 'Retry request_permissions with approve=true and confirm=true after user approval.'
        });
      }
      found.runtime.pendingOperations.delete(id);
      let result: JsonObject;
      try {
        result = await callTool(
          ctx,
          pending.name,
          { ...pending.args, confirm: true },
          pending.meta,
          true,
          processLifecycle,
          { folderId: pending.folderId, defaultCwd: pending.defaultCwd }
        );
      } catch (error) {
        found.runtime.pendingOperations.set(id, pending);
        throw error;
      }
      return {
        ...result,
        resumed: true,
        resume_id: id,
        resumed_workspace_folder_id: pending.folderId,
        resumed_execution_lane: PROCESS_TOOLS.has(pending.name) ? 'async_process' : 'blocking_worker',
        permission_grant: {
          status: 'granted_and_resumed',
          permission: pending.permission,
          reason: pending.reason,
          scope: String(args.scope ?? 'once')
        }
      };
    }
    case 'view_image': return viewImageTool(ctx, key, args);
    default: return fail('UNKNOWN_TOOL', `Unknown tool: ${name}`, 'catalog', true, {
      available_tools: toolNamesForProfile(ctx.config.activeToolProfile),
      toolset_revision: toolsetRevisionForProfile(ctx.config.activeToolProfile)
    });
  }
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
  override?: ToolBindingOverride
) {
  if (override) {
    const runtime = runtimeForFolderId(ctx, override.folderId);
    return { ctx, key, folderId: override.folderId, defaultCwd: override.defaultCwd ?? '.', runtime };
  }
  if (name === 'list_workspace_folders' || name === 'switch_workspace_folder') return { ctx, key };
  if (name === 'request_permissions') {
    const resumeId = String(args.resume_id ?? '').trim();
    if (resumeId) {
      const found = findPendingOperation(ctx, resumeId);
      if (found && found.operation.expiresAt < Date.now()) {
        found.runtime.pendingOperations.delete(resumeId);
      } else if (found) {
        return {
          ctx,
          key,
          folderId: found.operation.folderId,
          defaultCwd: found.operation.defaultCwd,
          runtime: found.runtime
        };
      }
    }
    return { ctx, key };
  }
  const folderId = ctx.selections.get(key);
  if (!folderId) return { ctx, key };
  return {
    ctx,
    key,
    folderId,
    defaultCwd: ctx.defaultCwds.get(key) ?? '.',
    runtime: runtimeForFolderId(ctx, folderId)
  };
}

function canonicalToolCall(name: string, args: JsonObject): { name: string; args: JsonObject } {
  if (name === 'edit_file') {
    const file: JsonObject = {};
    for (const field of ['path', 'expected_sha256', 'edits', 'apply_proposal']) {
      if (args[field] !== undefined) file[field] = args[field];
    }
    const converted: JsonObject = { files: [file] };
    if (args.dry_run !== undefined) converted.dry_run = args.dry_run;
    if (args.reason !== undefined) converted.reason = args.reason;
    return { name: 'edit', args: converted };
  }
  if (name === 'edit_many') return { name: 'edit', args };
  return { name, args };
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
  const canonical = canonicalToolCall(name, args);
  return runWithExecutionBinding(
    executionBindingFor(ctx, key, canonical.name, canonical.args, bindingOverride),
    () => callToolInScope(ctx, canonical.name, canonical.args, meta, skipPermission, processLifecycle)
  );
}

async function callToolInScope(
  ctx: ToolContext,
  name: string,
  args: JsonObject,
  meta: unknown,
  skipPermission = false,
  processLifecycle?: ProcessRequestLifecycle
): Promise<JsonObject> {
  const redaction = new OutputRedactionContext(name, args);
  const startedAt = Date.now();
  const requestTiming = skipPermission ? undefined : ctx.usageStore.beginRequest(startedAt);
  const key = ctx.conversations.identity(meta).key;
  const binding = currentExecutionBinding(ctx, key);
  const workspaceAdmission = binding?.runtime?.admission;
  const lockAdmission = workspaceAdmission ?? ctx.hubAdmission;
  const globalLane = PROCESS_TOOLS.has(name) ? ctx.hubAdmission.process : CONTROL_TOOLS.has(name) ? undefined : ctx.hubAdmission.blocking;
  const workspaceLane = PROCESS_TOOLS.has(name) ? workspaceAdmission?.process : CONTROL_TOOLS.has(name) ? undefined : workspaceAdmission?.blocking;
  let releaseGlobalLane: (() => void) | undefined;
  let releaseWorkspaceLane: (() => void) | undefined;
  let releaseLocks: (() => void) | undefined;
  let tracking: HarnessTracking | undefined;
  let trackingFinished = false;
  let queueWaitMs = 0;
  let globalAdmissionWaitMs = 0;
  let workspaceAdmissionWaitMs = 0;
  let lockWaitMs = 0;
  let result: JsonObject;
  const availableTools = toolNamesForProfile(ctx.config.activeToolProfile);
  const exposedTools = new Set(availableTools);
  const profileRevision = toolsetRevisionForProfile(ctx.config.activeToolProfile);

  const mapError = (error: unknown): JsonObject => toolErrorResult(error);

  if (!availableTools.includes(name)) {
    result = fail('UNKNOWN_TOOL', `Unknown tool: ${name}`, 'catalog', true, {
      reason: 'unknown_tool',
      suggestion: 'Refresh tools/list and retry with the current tool catalog.',
      tool_profile: ctx.config.activeToolProfile,
      toolset_revision: profileRevision,
      available_tools: availableTools
    });
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
        const groups = lockGroups(name);
        if (groups.length) {
          const lockStarted = Date.now();
          releaseLocks = await lockAdmission.locks.acquire(groups);
          lockWaitMs = Date.now() - lockStarted;
        }
        if (!HARNESS_TOOLS.has(name)) tracking = await beginHarnessTracking(ctx, key, name, args);
        try {
          result = await dispatch(ctx, name, args, meta, processLifecycle);
        } catch (error) {
          result = mapError(error);
        }
        if (tracking) {
          result = await finishHarnessTracking(ctx, key, name, args, tracking, result, exposedTools);
          trackingFinished = true;
        } else if (HARNESS_TOOLS.has(name) && result.ok === false) {
          result = await attachHarnessStatus(ctx, key, result, false, exposedTools);
        }
      }
    } catch (error) {
      result = mapError(error);
      if (tracking && !trackingFinished) {
        result = await finishHarnessTracking(ctx, key, name, args, tracking, result, exposedTools).catch(() => result);
      } else if (error instanceof HarnessError) {
        result = await attachHarnessStatus(ctx, key, result, false, exposedTools);
      }
    } finally {
      releaseLocks?.();
      releaseWorkspaceLane?.();
      releaseGlobalLane?.();
    }
  }

  const durationMs = Date.now() - startedAt;
  Object.assign(result, {
    execution_lane: PROCESS_TOOLS.has(name) ? 'process' : CONTROL_TOOLS.has(name) ? 'control' : 'blocking',
    admission_lane: PROCESS_TOOLS.has(name) ? 'process' : CONTROL_TOOLS.has(name) ? 'control' : 'blocking',
    admission_scope: workspaceLane ? 'global_and_workspace' : globalLane ? 'global' : 'none',
    admission_queue_wait_ms: queueWaitMs,
    global_admission_wait_ms: globalAdmissionWaitMs,
    workspace_admission_wait_ms: workspaceAdmissionWaitMs,
    workspace_lock_wait_ms: lockWaitMs,
    duration_ms: durationMs
  });
  result = normalizeToolResult(result);
  result = redaction.redact(result);
  const responseBytes = Buffer.byteLength(JSON.stringify(result));
  const folderId = binding?.folderId ?? selectedFolderId(ctx, key);
  if (requestTiming) {
    ctx.usageStore.recordToolCall({
      tool: name,
      arguments: args,
      result,
      startedTsMs: startedAt,
      durationMs,
      requestTiming,
      requestJsonBytes: Buffer.byteLength(JSON.stringify(args)),
      workspaceId: folderId
    });
  }
  ctx.usage.push({ tool: name, startedAt, durationMs, ok: result.ok === true, queueWaitMs, lockWaitMs, responseBytes });
  if (ctx.usage.length > 5_000) ctx.usage.splice(0, 1_000);
  return result;
}
