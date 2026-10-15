import { getBackend } from "$lib/backend";

export type TunnelService = "mcp" | "actions";

export interface TunnelStatus {
  state: string;
  publicUrl: string;
  tunnelPid: number | null;
  configuredWorkers: number | null;
  connectedWorkers: number | null;
  idleWorkers: number | null;
  busyWorkers: number | null;
  recycledWorkers: number | null;
  policyRevision: number | null;
  lastError: string | null;
  serverOrigin?: string | null;
  websocketUrl?: string | null;
  serverId?: string | null;
  deviceId?: string | null;
  transportMode?: string | null;
  controlConnected?: boolean | null;
  sharedSession?: boolean | null;
  sessionId?: string | null;
  routeId?: number | null;
  streamLimit?: number | null;
  dataConnections?: number | null;
  maxDataConnections?: number | null;
  streamsPerConnection?: number | null;
  activeStreams?: number | null;
  streamCapacity?: number | null;
  availableStreamSlots?: number | null;
  queuedRequests?: number | null;
  desiredDataConnections?: number | null;
}

export async function getFrpSnippet(id: string, service: TunnelService): Promise<string> {
  return getBackend().tunnel.getFrpSnippet(id, service);
}

export async function getTunnelStatus(id: string, service: TunnelService): Promise<TunnelStatus> {
  return getBackend().tunnel.status(id, service);
}

export async function startTunnel(id: string, service: TunnelService): Promise<TunnelStatus> {
  return getBackend().tunnel.start(id, service);
}

export async function stopTunnel(id: string, service: TunnelService): Promise<TunnelStatus> {
  return getBackend().tunnel.stop(id, service);
}

export interface TunnelTestResult {
  success: boolean;
  publicUrl: string;
  keptRunning: boolean;
  message: string;
}

export async function testTunnel(id: string, service: TunnelService): Promise<TunnelTestResult> {
  return getBackend().tunnel.test(id, service);
}

export async function restartTunnel(id: string, service: TunnelService): Promise<TunnelStatus> {
  return getBackend().tunnel.restart(id, service);
}
