import type { AgentConfig, SandboxConfig, SandboxPathGrant, WorkspaceFolder } from './types.js';
import type { ResolvedCommandSpec } from './policy.js';
import { dockerSbxHostAvailable, prepareDockerSbx, prepareDockerSbxLaunch } from './sandboxDockerSbx.js';
import { appContainerHostAvailable, prepareAppContainer, prepareAppContainerLaunch } from './sandboxAppContainer.js';
import { dockerHostAvailable, podmanHostAvailable, prepareOci, prepareOciLaunch } from './sandboxOci.js';
import { wslcHostAvailable } from './sandboxWslcProvisioner.js';
import { disposeWslc, prepareWslc, prepareWslcLaunch, WSLC_DEFAULT_IMAGE } from './sandboxWslc.js';
import { isWslUncPath } from './wsl.js';

export interface SandboxBackendOptionDescriptor {
  id: string;
  label: string;
  description: string;
  placeholder: string;
  defaultValue: string;
  required: boolean;
}

export interface SandboxBackendDescriptor {
  id: string;
  label: string;
  description: string;
  hostSupported: boolean;
  supportsWsl: boolean;
  enforcementReady: boolean;
  experimental: boolean;
  options: SandboxBackendOptionDescriptor[];
}

export type SandboxLaunchEnvironmentMode = 'command' | 'forwarded';

export interface SandboxLaunch {
  program: string;
  args: string[];
  environmentMode: SandboxLaunchEnvironmentMode;
  onSpawn?: (pid: number | undefined) => void;
  kill: () => Promise<void>;
  cleanup: () => Promise<void>;
  processTreeContained: boolean;
  processTreeControl: string;
}

export interface SandboxBoundary {
  enabled: boolean;
  backendId?: string;
  executionBoundary: string;
}

const DESCRIPTORS: readonly SandboxBackendDescriptor[] = [
  {
    id: 'appcontainer',
    label: 'Windows AppContainer',
    description: 'OS-enforced per-workspace isolation with a shared private runtime capability. Windows host folders only; WSL folders need Docker, Podman, Docker Sandboxes, or WSL Containers.',
    hostSupported: appContainerHostAvailable(),
    supportsWsl: false,
    enforcementReady: true,
    experimental: true,
    options: [
      {
        id: 'appcontainer.network',
        label: 'Network access',
        description: "AppContainer network capability. 'none' is the isolation-first default; 'internet' grants the well-known internetClient capability for package installs and clones.",
        placeholder: 'none',
        defaultValue: 'none',
        required: true
      }
    ]
  },
  {
    id: 'docker',
    label: 'Docker',
    description: 'Native Docker Linux containers. Each command runs in an ephemeral container with the workspace and explicit grants bind-mounted. Network is denied by default. Supports Windows host folders, WSL folders, Linux, and macOS when the Docker engine is running.',
    hostSupported: dockerHostAvailable(),
    supportsWsl: true,
    enforcementReady: true,
    experimental: true,
    options: [
      {
        id: 'docker.image',
        label: 'Container image',
        description: 'OCI image used for each isolated command container. The image must contain the tools you want to execute.',
        placeholder: 'ubuntu:24.04',
        defaultValue: 'ubuntu:24.04',
        required: true
      },
      {
        id: 'docker.network',
        label: 'Container network',
        description: "Docker network attached to command containers. 'none' is the isolation-first default; use 'bridge' or a named network when commands need networking. Host networking is rejected.",
        placeholder: 'none',
        defaultValue: 'none',
        required: true
      }
    ]
  },
  {
    id: 'podman',
    label: 'Podman',
    description: 'Native Podman Linux containers. Each command runs in an ephemeral container with the workspace and explicit grants bind-mounted. Network is denied by default. Supports Windows host folders, WSL folders, Linux, and macOS when the Podman engine is running.',
    hostSupported: podmanHostAvailable(),
    supportsWsl: true,
    enforcementReady: true,
    experimental: true,
    options: [
      {
        id: 'podman.image',
        label: 'Container image',
        description: 'OCI image used for each isolated command container. The image must contain the tools you want to execute.',
        placeholder: 'ubuntu:24.04',
        defaultValue: 'ubuntu:24.04',
        required: true
      },
      {
        id: 'podman.network',
        label: 'Container network',
        description: "Podman network attached to command containers. 'none' is the isolation-first default; use 'bridge' or a named network when commands need networking. Host networking is rejected.",
        placeholder: 'none',
        defaultValue: 'none',
        required: true
      }
    ]
  },
  {
    id: 'docker_sbx',
    label: 'Docker Sandboxes (sbx)',
    description: 'Linux microVM sandbox using Docker sbx. The primary workspace is a direct read-write mount; explicit external directories honor read-only/modify grants. Supports Windows host folders and WSL folders via WSL UNC mounts.',
    hostSupported: dockerSbxHostAvailable(),
    supportsWsl: true,
    enforcementReady: true,
    experimental: true,
    options: []
  },
  {
    id: 'wslc',
    label: 'Microsoft WSL Containers (wslc)',
    description: 'Microsoft first-party Linux container sandbox managed by wslc. Host paths are exposed only through explicit bind mounts. Supports Windows host folders and WSL folders via WSL UNC mounts.',
    hostSupported: wslcHostAvailable(),
    supportsWsl: true,
    enforcementReady: true,
    experimental: true,
    options: [
      {
        id: 'wslc.image',
        label: 'Container image',
        description: 'OCI image used for each isolated command container. The image must contain the tools you want to execute.',
        placeholder: WSLC_DEFAULT_IMAGE,
        defaultValue: WSLC_DEFAULT_IMAGE,
        required: true
      },
      {
        id: 'wslc.network',
        label: 'Container network',
        description: "WSLC network attached to command containers. 'none' is the isolation-first default; use 'bridge' or a named network when commands need networking.",
        placeholder: 'none',
        defaultValue: 'none',
        required: true
      }
    ]
  }
] as const;

export class SandboxBoundaryError extends Error {
  readonly code: string;
  readonly category = 'security';
  readonly retryable = false;
  readonly details: Record<string, unknown>;

  constructor(code: string, message: string, backendId: string, stage = 'prepare') {
    super(message);
    this.name = 'SandboxBoundaryError';
    this.code = code;
    this.details = {
      sandbox_backend: backendId,
      stage,
      fallback_allowed: false
    };
  }
}

export function sandboxBackends(): SandboxBackendDescriptor[] {
  return DESCRIPTORS.map(descriptor => ({
    ...descriptor,
    options: descriptor.options.map(option => ({ ...option }))
  }));
}

export function sandboxBackend(id: string): SandboxBackendDescriptor | undefined {
  const normalized = id.trim();
  const descriptor = DESCRIPTORS.find(candidate => candidate.id === normalized);
  return descriptor
    ? { ...descriptor, options: descriptor.options.map(option => ({ ...option })) }
    : undefined;
}

export function normalizedSandboxConfig(config: SandboxConfig | undefined): SandboxConfig {
  return config ?? {
    enabled: false,
    backend: 'appcontainer',
    externalPaths: [],
    options: {}
  };
}

export function sandboxUsesPortableCommand(backendId: string | undefined): boolean {
  return backendId === 'docker' || backendId === 'podman' || backendId === 'docker_sbx' || backendId === 'wslc';
}

export function sandboxAvailable(config: SandboxConfig | undefined): boolean {
  const normalized = normalizedSandboxConfig(config);
  const descriptor = sandboxBackend(normalized.backend);
  return Boolean(descriptor?.hostSupported && descriptor.enforcementReady);
}

export function sandboxBoundary(config: AgentConfig['sandbox'] | undefined, _folder?: WorkspaceFolder): SandboxBoundary {
  const normalized = normalizedSandboxConfig(config);
  if (!normalized.enabled) return { enabled: false, executionBoundary: 'policy_only' };
  const backendId = normalized.backend.trim();
  const descriptor = sandboxBackend(backendId);
  if (!descriptor) {
    throw new SandboxBoundaryError(
      'SANDBOX_BACKEND_UNKNOWN',
      `Unknown sandbox backend: ${backendId || '<empty>'}`,
      backendId || '<empty>'
    );
  }
  if (!descriptor.hostSupported) {
    throw new SandboxBoundaryError(
      'SANDBOX_BACKEND_UNSUPPORTED',
      `Sandbox backend is not supported on this host: ${backendId}`,
      backendId
    );
  }
  if (!descriptor.enforcementReady) {
    throw new SandboxBoundaryError(
      'SANDBOX_BACKEND_NOT_READY',
      `Sandbox backend is not ready in the Node Agent runtime: ${backendId}`,
      backendId
    );
  }
  if (!descriptor.supportsWsl && _folder && isWslUncPath(_folder.path)) {
    throw new SandboxBoundaryError(
      'SANDBOX_BACKEND_UNSUPPORTED',
      `Sandbox backend is not supported for WSL folders: ${backendId}. Use Docker, Podman, Docker Sandboxes, or WSL Containers.`,
      backendId
    );
  }
  return { enabled: true, backendId, executionBoundary: backendId };
}

export async function preflightSandboxConfiguration(
  config: SandboxConfig,
  folders: readonly WorkspaceFolder[],
  dataDir: string
): Promise<void> {
  if (!config.enabled) return;
  for (const folder of folders) {
    sandboxBoundary(config, folder);
    switch (config.backend.trim()) {
      case 'appcontainer':
        await prepareAppContainer(config, folder.path, dataDir);
        break;
      case 'docker':
        await prepareOci('docker', config, folder.path);
        break;
      case 'podman':
        await prepareOci('podman', config, folder.path);
        break;
      case 'docker_sbx':
        await prepareDockerSbx(config, folder.path);
        break;
      case 'wslc': {
        const prepared = await prepareWslc(config, folder.path, dataDir);
        await disposeWslc(prepared);
        break;
      }
      default:
        throw new SandboxBoundaryError(
          'SANDBOX_BACKEND_UNKNOWN',
          `Unknown sandbox backend: ${config.backend.trim() || '<empty>'}`,
          config.backend.trim() || '<empty>'
        );
    }
  }
}

export async function prepareSandboxLaunch(
  config: SandboxConfig,
  workspaceRoot: string,
  dataDir: string,
  cwd: string,
  spec: ResolvedCommandSpec,
  environment: Array<[string, string]>,
  removeEnvironment: string[],
  signal?: AbortSignal,
  timeoutMs = 30_000
): Promise<SandboxLaunch> {
  switch (config.backend.trim()) {
    case 'appcontainer': {
      const prepared = await prepareAppContainer(config, workspaceRoot, dataDir);
      return prepareAppContainerLaunch(prepared, cwd, spec, environment, removeEnvironment);
    }
    case 'docker': {
      const prepared = await prepareOci('docker', config, workspaceRoot);
      return prepareOciLaunch(prepared, cwd, spec, environment, removeEnvironment);
    }
    case 'podman': {
      const prepared = await prepareOci('podman', config, workspaceRoot);
      return prepareOciLaunch(prepared, cwd, spec, environment, removeEnvironment);
    }
    case 'docker_sbx': {
      const prepared = await prepareDockerSbx(config, workspaceRoot);
      return prepareDockerSbxLaunch(prepared, cwd, spec, environment, removeEnvironment);
    }
    case 'wslc': {
      const prepared = await prepareWslc(config, workspaceRoot, dataDir, signal, timeoutMs);
      return prepareWslcLaunch(prepared, cwd, spec, environment, removeEnvironment);
    }
    default:
      throw new SandboxBoundaryError(
        'SANDBOX_BACKEND_UNKNOWN',
        `Unknown sandbox backend: ${config.backend.trim() || '<empty>'}`,
        config.backend.trim() || '<empty>'
      );
  }
}

export function sandboxPathGrants(config: SandboxConfig): SandboxPathGrant[] {
  return config.externalPaths.map(grant => ({ ...grant }));
}
