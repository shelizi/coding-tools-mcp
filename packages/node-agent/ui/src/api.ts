import type {
  ConfigSaveResult,
  ConfigSnapshot,
  ConfigUpdatePayload,
  DashboardPayload,
  DiagnosticsPayload,
  HealthCheckPayload,
  HistoryDetailPayload,
  HistoryListPayload,
  ManagementStatus,
  OperationLogFilters,
  OperationLogPayload,
  RestartResult,
  SecretResult,
  TelemetryFilters,
  TelemetryPayload
} from './types';

function adminToken(): string {
  const token = document.querySelector<HTMLMetaElement>('meta[name="ctmcp-admin-token"]')?.content;
  if (!token) throw new Error('管理介面 token 遺失，請重新載入頁面。');
  return token;
}

async function request<T>(route: string, init: RequestInit = {}, signal?: AbortSignal): Promise<T> {
  const headers = new Headers(init.headers);
  headers.set('x-ctmcp-admin-token', adminToken());
  if (init.body) headers.set('content-type', 'application/json');
  const response = await fetch(route, {
    ...init,
    headers,
    signal,
    cache: 'no-store',
    credentials: 'same-origin'
  });
  const data = await response.json().catch(() => ({ error: { message: '伺服器回應格式錯誤。' } }));
  if (!response.ok) {
    const message = data?.error?.message ?? data?.error ?? response.statusText;
    throw new Error(String(message));
  }
  return data as T;
}

function workspaceRoute(workspaceId: string, suffix: string): string {
  return `/admin/api/workspaces/${encodeURIComponent(workspaceId)}/${suffix}`;
}

export function fetchStatus(signal?: AbortSignal): Promise<ManagementStatus> {
  return request('/admin/api/status', {}, signal);
}

export function fetchDashboard(workspaceId?: string, signal?: AbortSignal): Promise<DashboardPayload> {
  const query = workspaceId ? `?workspaceId=${encodeURIComponent(workspaceId)}` : '';
  return request(`/admin/api/dashboard${query}`, {}, signal);
}

export function fetchConfig(signal?: AbortSignal): Promise<ConfigSnapshot> {
  return request('/admin/api/config', {}, signal);
}

export function saveWorkspaceConfig(workspaceId: string, payload: ConfigUpdatePayload): Promise<ConfigSaveResult> {
  return request(workspaceRoute(workspaceId, 'config'), { method: 'PUT', body: JSON.stringify(payload) });
}

export function fetchWorkspacePassword(workspaceId: string, signal?: AbortSignal): Promise<SecretResult> {
  return request(workspaceRoute(workspaceId, 'secrets/oauth-password'), {}, signal);
}

export function regenerateWorkspacePassword(workspaceId: string): Promise<SecretResult> {
  return request(workspaceRoute(workspaceId, 'secrets/oauth-password/regenerate'), { method: 'POST' });
}

export function restartAgent(): Promise<RestartResult> {
  return request('/admin/api/restart', { method: 'POST' });
}

export function fetchWorkspaceTelemetry(
  workspaceId: string,
  filters: TelemetryFilters,
  signal?: AbortSignal
): Promise<TelemetryPayload> {
  const query = new URLSearchParams({
    scope: filters.scope,
    errorsOnly: String(filters.errorsOnly),
    limit: String(filters.limit),
    minDurationMs: String(filters.minDurationMs),
    sortBy: filters.sortBy
  });
  return request(`${workspaceRoute(workspaceId, 'telemetry')}?${query}`, {}, signal);
}

export function fetchWorkspaceOperationLogs(
  workspaceId: string,
  filters: OperationLogFilters,
  cursor = 0,
  signal?: AbortSignal
): Promise<OperationLogPayload> {
  const query = new URLSearchParams({
    folderId: filters.folderId,
    status: filters.status,
    tool: filters.tool,
    errorsOnly: String(filters.errorsOnly),
    limit: String(filters.limit),
    cursor: String(cursor)
  });
  return request(`${workspaceRoute(workspaceId, 'logs')}?${query}`, {}, signal);
}

export function fetchWorkspaceHistory(
  workspaceId: string,
  folderId: string,
  signal?: AbortSignal
): Promise<HistoryListPayload> {
  const query = new URLSearchParams({ folderId });
  return request(`${workspaceRoute(workspaceId, 'history')}?${query}`, {}, signal);
}

export function fetchWorkspaceHistorySession(
  workspaceId: string,
  folderId: string,
  sessionNumber: number,
  signal?: AbortSignal
): Promise<HistoryDetailPayload> {
  const query = new URLSearchParams({ folderId });
  return request(`${workspaceRoute(workspaceId, `history/${sessionNumber}`)}?${query}`, {}, signal);
}

export function runWorkspaceHealth(workspaceId: string, signal?: AbortSignal): Promise<HealthCheckPayload> {
  return request(workspaceRoute(workspaceId, 'health'), { method: 'POST' }, signal);
}

export function fetchWorkspaceDiagnostics(workspaceId: string, signal?: AbortSignal): Promise<DiagnosticsPayload> {
  return request(workspaceRoute(workspaceId, 'diagnostics'), {}, signal);
}
