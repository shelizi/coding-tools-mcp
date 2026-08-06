#!/usr/bin/env node
import { ApplicationConfigStore, loadApplication } from './application.js';
import { CURRENT_CONFIG_SCHEMA_VERSION } from './config.js';
import type { WorkspaceRuntimeRecord } from './management.js';
import { createAgentRuntime, type AgentRuntime } from './server.js';
import { AGENT_VERSION, CLIENT_COMPAT_VERSION } from './version.js';
import { BuiltinTunnelManager } from './tunnel.js';

const configIndex = process.argv.indexOf('--config');
if (configIndex >= 0 && !process.argv[configIndex + 1]) throw new Error('--config requires a file path');
const configPath = configIndex >= 0 ? process.argv[configIndex + 1] : undefined;
const noUi = process.argv.includes('--no-ui');
const application = await loadApplication(configPath);
const workspaceStore = new ApplicationConfigStore(application);
const runtimeRegistry = new Map<string, WorkspaceRuntimeRecord>();

const RESTART_EXIT_CODE = 75;
const restartSupervised = process.env.CTMCP_RESTART_SUPERVISED === '1';
interface RunningWorkspace {
  id: string;
  name: string;
  primary: boolean;
  runtime: AgentRuntime;
  tunnel: BuiltinTunnelManager;
}
const running: RunningWorkspace[] = [];
let stopping = false;
let exitRequested = false;

async function closeRuntime(item: RunningWorkspace): Promise<void> {
  await item.tunnel.stop();
  if (!item.runtime.server.listening) return;
  await new Promise<void>((resolve, reject) => {
    item.runtime.server.close(error => error ? reject(error) : resolve());
  });
}

async function stop(): Promise<void> {
  if (stopping) return;
  stopping = true;
  const results = await Promise.allSettled(running.map(closeRuntime));
  const errors = results
    .filter((result): result is PromiseRejectedResult => result.status === 'rejected')
    .map(result => result.reason);
  if (errors.length) throw new AggregateError(errors, 'One or more workspace runtimes failed to stop cleanly.');
}

const requestRestart = restartSupervised ? () => {
  if (exitRequested) return;
  exitRequested = true;
  console.log('Restart requested from the management UI.');
  void stop().finally(() => process.exit(RESTART_EXIT_CODE));
} : undefined;

try {
  for (const workspace of application.workspaces) {
    const primary = workspace.id === workspaceStore.primaryWorkspaceId;
    const config = structuredClone(workspace.loaded.config);
    config.management.enabled = primary && !noUi && config.management.enabled;
    if (config.oauth.password === 'change-me') {
      console.warn(`[${workspace.name}] Warning: CTMCP_OAUTH_PASSWORD is using the development default.`);
    }
    const configStore = workspaceStore.workspace(workspace.id).store;
    const runtime = await createAgentRuntime(config, {
      ...(primary ? { configStore, workspaceStore } : {}),
      ...(primary && requestRestart ? { requestRestart } : {}),
      runtimeRegistry
    });
    const tunnel = new BuiltinTunnelManager(config, runtime.context, {
      onEndpointResolved: endpoint => workspaceStore.applyResolvedBuiltinTunnel(
        workspace.id,
        endpoint.publicUrl,
        endpoint.enrollmentCompleted
      )
    });
    const item = { id: workspace.id, name: workspace.name, primary, runtime, tunnel };
    running.push(item);

    await new Promise<void>((resolve, reject) => {
      runtime.server.once('error', reject);
      runtime.server.listen(config.port, config.host, () => {
        runtime.server.off('error', reject);
        resolve();
      });
    });

    try {
      await tunnel.start();
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      console.error(`[${workspace.name}] Built-in WSS failed to start: ${message}`);
      console.error(`[${workspace.name}] Local MCP remains available so the tunnel configuration can be corrected.`);
    }
  }
} catch (error) {
  await stop().catch(stopError => console.error(stopError));
  throw error;
}

console.log(`Coding Tools MCP Node Agent ${AGENT_VERSION}`);
console.log(`Desktop Client compatibility: ${CLIENT_COMPAT_VERSION}`);
console.log(`Workspace registry: ${application.registryPath}`);
console.log(`Workspace profiles: ${running.length}`);
for (const item of running) {
  const workspace = workspaceStore.workspace(item.id);
  const config = item.runtime.context.config;
  console.log(`\n[${item.name}] ${item.id}`);
  console.log(`MCP: http://${config.host}:${config.port}/mcp`);
  console.log(`OAuth metadata: http://${config.host}:${config.port}/.well-known/oauth-authorization-server`);
  console.log(`Configuration file: ${workspace.store.configPath}`);
  console.log(`Config schema: v${CURRENT_CONFIG_SCHEMA_VERSION}`);
  console.log(`Folders: ${config.folders.map(folder => `${folder.name}=${folder.path}`).join(', ')}`);
  console.log(`Tool profile: ${config.activeToolProfile} (configured: ${config.toolProfile})`);
  if (item.primary && config.management.enabled) {
    console.log(`Management UI: http://127.0.0.1:${config.port}/ui (all workspaces, loopback only)`);
  }
  if (config.tunnel?.enabled) {
    if (item.runtime.context.tunnelStatus?.state === 'error') {
      console.log(`Built-in WSS: error (${item.runtime.context.tunnelStatus.lastError ?? 'unknown error'})`);
    } else {
      console.log(`Built-in WSS: ${config.tunnel.publicUrl} (dynamic WorkerPolicy)`);
    }
  }
}
console.log(`\nWeb UI restart: ${restartSupervised ? 'enabled by supervisor' : 'unavailable (start with start-node-agent.bat)'}`);
console.log('FRP and Cloudflare transports are disabled in the Node Agent.');

for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, () => {
    if (exitRequested) return;
    exitRequested = true;
    void stop().finally(() => process.exit(0));
  });
}
