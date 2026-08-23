import { toolNamesForProfile, toolsetRevisionForProfile } from '../catalog.js';
import { allFolderRuntimes } from '../folderRuntime.js';
import type { JsonObject } from '../types.js';
import { AGENT_VERSION } from '../version.js';
import type { ManagementOptions, WorkspaceRuntimeRecord } from './types.js';

export function runtimeRecords(options: ManagementOptions): Array<[string, WorkspaceRuntimeRecord]> {
  if (options.runtimeRegistry?.size) return [...options.runtimeRegistry.entries()];
  const id = options.context.config.workspaceId ?? options.context.workspaceProfileId;
  return [[id, { context: options.context, oauth: options.oauth, startedAt: options.startedAt }]];
}

export function runtimeRecord(
  options: ManagementOptions,
  workspaceId: string
): WorkspaceRuntimeRecord | undefined {
  const registered = options.runtimeRegistry?.get(workspaceId);
  if (registered) return registered;
  const currentId = options.context.config.workspaceId ?? options.context.workspaceProfileId;
  return workspaceId === currentId
    ? { context: options.context, oauth: options.oauth, startedAt: options.startedAt }
    : undefined;
}

export function statusPayload(options: ManagementOptions): JsonObject {
  const records = runtimeRecords(options);
  const primaryId = options.workspaceStore?.primaryWorkspaceId
    ?? options.context.config.workspaceId
    ?? options.context.workspaceProfileId;
  const primary = options.runtimeRegistry?.get(primaryId)
    ?? { context: options.context, oauth: options.oauth, startedAt: options.startedAt };
  const sessions = records.flatMap(([, record]) => (
    allFolderRuntimes(record.context).flatMap(runtime => [...runtime.sessions.values()])
  ));
  const toolProfile = primary.context.config.activeToolProfile;
  const profileTools = toolNamesForProfile(toolProfile);
  return {
    ok: true,
    version: AGENT_VERSION,
    uptimeMs: Date.now() - Math.min(...records.map(([, record]) => record.startedAt)),
    tools: profileTools.length,
    toolProfile,
    configuredToolProfile: primary.context.config.toolProfile,
    toolsetRevision: toolsetRevisionForProfile(toolProfile),
    workspaces: records.map(([id, record]) => ({
      id,
      name: record.context.config.workspaceName ?? id,
      host: record.context.config.host,
      port: record.context.config.port,
      folders: record.context.config.folders,
      permissionMode: record.context.config.permissionMode,
      toolProfile: record.context.config.activeToolProfile,
      tunnel: record.context.tunnelStatus
    })),
    permissionMode: primary.context.config.permissionMode,
    sessions: {
      total: sessions.length,
      running: sessions.filter(session => !session.endedAt).length,
      finalized: sessions.filter(session => Boolean(session.finalizedAt)).length
    },
    tunnel: primary.context.tunnelStatus,
    restart: {
      supported: Boolean(options.requestRestart),
      mode: options.requestRestart ? 'supervised' : 'unavailable'
    },
    configPath: options.configStore.configPath,
    headless: true
  };
}
