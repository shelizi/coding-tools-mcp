import { randomBytes, randomUUID } from 'node:crypto';
import { chmod, mkdir, readFile, rename, writeFile } from 'node:fs/promises';
import path from 'node:path';
import { loadConfigBundle, resolveConfigPath, type LoadedConfig } from './config.js';
import { ConfigStore, type RuntimeHotApplyTarget } from './management.js';
import { generateAuthorizationPassword } from './secrets.js';
import type { JsonObject, WorkspaceRegistryDocument } from './types.js';

export interface LoadedWorkspaceProfile {
  id: string;
  name: string;
  loaded: LoadedConfig;
}

export interface LoadedApplication {
  registryPath: string;
  registry: WorkspaceRegistryDocument;
  workspaces: LoadedWorkspaceProfile[];
  registryCreated: boolean;
}

interface ManagedWorkspaceProfile {
  id: string;
  name: string;
  store: ConfigStore;
}

function workspaceId(): string {
  return randomUUID().replaceAll('-', '');
}

function inferredWorkspaceName(loaded: LoadedConfig): string {
  const folders = loaded.config.folders;
  if (folders.length === 1 && folders[0]?.name.trim()) return folders[0].name.trim();
  return path.basename(path.dirname(loaded.configPath)) || 'Workspace';
}

function registryPathFor(configPath: string): string {
  return path.join(path.dirname(configPath), 'workspace-profiles.json');
}

function validateRegistry(value: unknown, registryPath: string): WorkspaceRegistryDocument {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error(`Workspace registry must be an object: ${registryPath}`);
  }
  const input = value as Record<string, unknown>;
  if (input.schema_version !== 1) throw new Error(`Workspace registry schema_version must be 1: ${registryPath}`);
  if (!Array.isArray(input.workspaces) || input.workspaces.length < 1 || input.workspaces.length > 100) {
    throw new Error(`Workspace registry must contain 1 to 100 workspaces: ${registryPath}`);
  }
  const ids = new Set<string>();
  const configPaths = new Set<string>();
  const base = path.dirname(registryPath);
  const workspaces = input.workspaces.map((raw, index) => {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new Error(`workspaces[${index}] must be an object`);
    const entry = raw as Record<string, unknown>;
    const id = String(entry.id ?? '').trim();
    const name = String(entry.name ?? '').trim();
    const rawConfigPath = String(entry.configPath ?? '').trim();
    if (!rawConfigPath) throw new Error(`workspaces[${index}].configPath is required`);
    const configPath = path.resolve(base, rawConfigPath);
    if (!/^[A-Za-z0-9._-]{1,128}$/.test(id)) throw new Error(`workspaces[${index}].id is invalid`);
    if (!name || name.length > 128) throw new Error(`workspaces[${index}].name is invalid`);
    const comparable = process.platform === 'win32' ? configPath.toLowerCase() : configPath;
    if (ids.has(id)) throw new Error(`Workspace registry ID is duplicated: ${id}`);
    if (configPaths.has(comparable)) throw new Error(`Workspace registry config path is duplicated: ${configPath}`);
    ids.add(id);
    configPaths.add(comparable);
    return { id, name, configPath };
  });
  return { schema_version: 1, workspaces };
}

async function writeRegistry(registryPath: string, registry: WorkspaceRegistryDocument): Promise<void> {
  const validated = validateRegistry(registry, registryPath);
  await mkdir(path.dirname(registryPath), { recursive: true, mode: 0o700 });
  const temporary = `${registryPath}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`;
  await writeFile(temporary, `${JSON.stringify(validated, null, 2)}\n`, { flag: 'wx', mode: 0o600 });
  await rename(temporary, registryPath);
  await chmod(registryPath, 0o600).catch(() => undefined);
}

async function readRegistry(registryPath: string): Promise<WorkspaceRegistryDocument | undefined> {
  try {
    return validateRegistry(JSON.parse(await readFile(registryPath, 'utf8')), registryPath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

export async function loadApplication(configFile?: string): Promise<LoadedApplication> {
  const primaryConfigPath = resolveConfigPath(configFile);
  const primary = await loadConfigBundle(primaryConfigPath);
  const registryPath = registryPathFor(primaryConfigPath);
  let registry = await readRegistry(registryPath);
  let registryCreated = false;
  if (!registry) {
    registry = {
      schema_version: 1,
      workspaces: [{ id: workspaceId(), name: inferredWorkspaceName(primary), configPath: primaryConfigPath }]
    };
    await writeRegistry(registryPath, registry);
    registryCreated = true;
  }

  const primaryComparable = process.platform === 'win32' ? primaryConfigPath.toLowerCase() : primaryConfigPath;
  const workspaces: LoadedWorkspaceProfile[] = [];
  const ports = new Map<string, string>();
  for (const entry of registry.workspaces) {
    const comparable = process.platform === 'win32' ? entry.configPath.toLowerCase() : entry.configPath;
    const loaded = comparable === primaryComparable ? primary : await loadConfigBundle(entry.configPath);
    loaded.config.workspaceId = entry.id;
    loaded.config.workspaceName = entry.name;
    const address = `${loaded.config.host}:${loaded.config.port}`.toLowerCase();
    const conflict = ports.get(address);
    if (conflict) throw new Error(`Workspace ports conflict: ${conflict} and ${entry.name} both use ${address}`);
    ports.set(address, entry.name);
    workspaces.push({ id: entry.id, name: entry.name, loaded });
  }
  return { registryPath, registry, workspaces, registryCreated };
}

export class ApplicationConfigStore {
  readonly registryPath: string;
  readonly primaryWorkspaceId: string;
  private registry: WorkspaceRegistryDocument;
  private readonly workspaces = new Map<string, ManagedWorkspaceProfile>();

  constructor(application: LoadedApplication) {
    this.registryPath = application.registryPath;
    this.registry = structuredClone(application.registry);
    this.primaryWorkspaceId = application.workspaces[0]?.id ?? '';
    for (const workspace of application.workspaces) {
      this.workspaces.set(workspace.id, {
        id: workspace.id,
        name: workspace.name,
        store: new ConfigStore(workspace.loaded)
      });
    }
  }

  entries(): Array<{ id: string; name: string; store: ConfigStore }> {
    return this.registry.workspaces.map(entry => {
      const workspace = this.workspaces.get(entry.id);
      if (!workspace) throw new Error(`Workspace registry entry is not loaded: ${entry.id}`);
      return workspace;
    });
  }

  snapshot(): JsonObject {
    return {
      schemaVersion: 1,
      registryPath: this.registryPath,
      primaryWorkspaceId: this.primaryWorkspaceId,
      workspaces: this.entries().map(workspace => ({
        id: workspace.id,
        name: workspace.name,
        ...workspace.store.snapshot()
      }))
    };
  }

  workspace(id: string): { id: string; name: string; store: ConfigStore } {
    const workspace = this.workspaces.get(id);
    if (!workspace) throw new Error(`Workspace was not found: ${id}`);
    return workspace;
  }

  async saveWorkspace(id: string, value: unknown, runtime?: RuntimeHotApplyTarget): Promise<JsonObject> {
    const workspace = this.workspace(id);
    const input = value && typeof value === 'object' && !Array.isArray(value)
      ? value as Record<string, unknown>
      : {};
    const name = String(input.name ?? workspace.name).trim();
    if (!name || name.length > 128) throw new Error('workspace name must contain 1 to 128 characters');
    const result = await workspace.store.save(input, runtime);
    if (name !== workspace.name) {
      workspace.name = name;
      const entry = this.registry.workspaces.find(item => item.id === id);
      if (!entry) throw new Error(`Workspace registry entry is missing: ${id}`);
      entry.name = name;
      await writeRegistry(this.registryPath, this.registry);
      workspace.store.current.workspaceName = name;
      if (runtime) runtime.context.config.workspaceName = name;
    }
    return { ...result, id, name };
  }

  secret(id: string, key: 'oauthPassword'): string {
    return this.workspace(id).store.secret(key);
  }

  async regenerateSecret(id: string, key: 'oauthPassword', runtime?: RuntimeHotApplyTarget): Promise<JsonObject> {
    const value = key === 'oauthPassword' ? generateAuthorizationPassword() : '';
    const workspace = this.workspace(id);
    const applied = await workspace.store.replaceSecret(key, value, runtime);
    return {
      ok: true,
      workspaceId: id,
      key,
      value,
      restartRequired: !applied,
      appliedImmediately: applied ? ['oauth'] : []
    };
  }

  applyResolvedBuiltinTunnel(id: string, publicUrl: string, enrollmentCompleted: boolean): Promise<void> {
    return this.workspace(id).store.applyResolvedBuiltinTunnel(publicUrl, enrollmentCompleted);
  }
}
