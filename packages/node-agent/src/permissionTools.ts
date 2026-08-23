import { randomUUID } from 'node:crypto';
import { readOnlyTools } from './catalog.js';
import { currentFolderRuntime, findPendingOperation } from './folderRuntime.js';
import type { ToolDispatchRequest, ToolHandlerMap } from './toolDispatch/contract.js';
import { toolFail } from './toolContract.js';
import { toolRuntimeFor } from './toolRuntime.js';
import type { FolderRuntime, JsonObject, PendingOperation, ToolContext } from './types.js';
import { relativeInside, rootAndCwd, selectedFolderSafe } from './workspace.js';

const MAX_PENDING_OPERATIONS = 256;
const fail = toolFail;

export interface PendingPermissionBinding {
  readonly folderId: string;
  readonly defaultCwd: string;
  readonly runtime: FolderRuntime;
}

function ok(value: JsonObject = {}): JsonObject {
  return { ok: true, ...value };
}

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

export function pendingPermissionBinding(ctx: ToolContext, args: JsonObject): PendingPermissionBinding | undefined {
  const resumeId = String(args.resume_id ?? '').trim();
  if (!resumeId) return undefined;
  const found = findPendingOperation(ctx, resumeId);
  if (!found) return undefined;
  if (found.operation.expiresAt < Date.now()) {
    found.runtime.pendingOperations.delete(resumeId);
    return undefined;
  }
  return {
    folderId: found.operation.folderId,
    defaultCwd: found.operation.defaultCwd,
    runtime: found.runtime
  };
}

export function permissionDecision(
  ctx: ToolContext,
  key: string,
  name: string,
  args: JsonObject,
  meta: unknown
): JsonObject | undefined {
  if (name === 'request_permissions' || name === 'list_workspace_folders' || name === 'switch_workspace_folder' || name === 'conversation_bootstrap') return undefined;
  if (ctx.config.securityPolicyCustomized) return undefined;
  if (ctx.config.permissionMode === 'read-only' && !readOnlyTools.has(name)) {
    return fail('PERMISSION_MODE_READ_ONLY', `${name} is unavailable in read-only mode`, 'policy', false, { permission_mode: 'read-only' });
  }
  const runtimePolicy = toolRuntimeFor(name);
  if (ctx.config.permissionMode !== 'guarded' || !runtimePolicy.guardedPermission || args.confirm === true) return undefined;
  const runtime = currentFolderRuntime(ctx, key);
  const { folder, root, cwd } = rootAndCwd(ctx, key);
  preparePendingInsert(runtime);
  const resumeId = randomUUID();
  const permission = runtimePolicy.guardedPermission;
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

async function requestPermissions({ ctx, key, args, resumeTool }: ToolDispatchRequest): Promise<JsonObject> {
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
  if (!resumeTool) throw new Error('request_permissions requires a resume callback');
  found.runtime.pendingOperations.delete(id);
  let result: JsonObject;
  try {
    result = await resumeTool({
      name: pending.name,
      args: { ...pending.args, confirm: true },
      meta: pending.meta,
      folderId: pending.folderId,
      defaultCwd: pending.defaultCwd
    });
  } catch (error) {
    found.runtime.pendingOperations.set(id, pending);
    throw error;
  }
  return {
    ...result,
    resumed: true,
    resume_id: id,
    resumed_workspace_folder_id: pending.folderId,
    resumed_execution_lane: toolRuntimeFor(pending.name).lane === 'process' ? 'async_process' : 'blocking_worker',
    permission_grant: {
      status: 'granted_and_resumed',
      permission: pending.permission,
      reason: pending.reason,
      scope: String(args.scope ?? 'once')
    }
  };
}

export const permissionToolHandlers = {
  request_permissions: requestPermissions
} satisfies ToolHandlerMap;
