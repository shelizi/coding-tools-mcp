import type { HistorySessionSummary, HistorySessionsResult } from "../api/history";

export interface HistoryLocation {
  key: string;
  workspaceId: string;
  workspaceName: string;
  folderId: string;
  folderName: string;
  folderPath: string;
}

export interface LocatedHistorySession extends HistorySessionSummary {
  key: string;
  workspaceId: string;
  workspaceName: string;
  folderId: string;
  folderName: string;
  folderPath: string;
}

export function historyLocationKey(workspaceId: string, folderId: string): string {
  // Numbers are only unique within a folder. JSON tuples avoid delimiter collisions.
  return JSON.stringify([workspaceId, folderId]);
}

export function historySessionKey(workspaceId: string, folderId: string, number: number): string {
  return JSON.stringify([workspaceId, folderId, number]);
}

export function uniqueHistoryLocations(locations: readonly HistoryLocation[]): HistoryLocation[] {
  const paths = new Set<string>();
  return locations.filter((location) => {
    // The same physical folder can be configured under multiple workspace profiles.
    // Only read it once so "All projects" never doubles its history.
    const normalized = location.folderPath.replace(/\\/g, "/").replace(/\/+$/, "");
    // Windows drive and UNC paths are case-insensitive; Unix/WSL paths are not.
    const path = /^[a-z]:\//i.test(normalized) || normalized.startsWith("//")
      ? normalized.toLowerCase()
      : normalized;
    if (!path) return true;
    if (paths.has(path)) return false;
    paths.add(path);
    return true;
  });
}

export function historyTimestamp(session: HistorySessionSummary): number {
  if (session.lastActivityAtMs !== null) return session.lastActivityAtMs;
  if (!session.updatedAt) return 0;
  if (session.updatedAt.startsWith("unix:")) {
    return Number(session.updatedAt.slice(5)) * 1_000 || 0;
  }
  return Date.parse(session.updatedAt) || 0;
}

export function sortLocatedHistory(items: readonly LocatedHistorySession[]): LocatedHistorySession[] {
  const priority = { running: 0, active: 1, inactive: 2, completed: 3 };
  return [...items].sort((left, right) => (
    priority[left.activityStatus] - priority[right.activityStatus] ||
    historyTimestamp(right) - historyTimestamp(left) ||
    right.number - left.number ||
    left.key.localeCompare(right.key)
  ));
}

export async function collectHistory(
  locations: readonly HistoryLocation[],
  list: (workspaceId: string, folderId: string) => Promise<HistorySessionsResult>,
  maxConcurrent = 4,
  signal?: AbortSignal,
): Promise<{
  sessions: LocatedHistorySession[];
  failed: HistoryLocation[];
  succeeded: number;
}> {
  const sessions: LocatedHistorySession[] = [];
  const failed: HistoryLocation[] = [];
  let succeeded = 0;
  let next = 0;
  const workers = Array.from(
    { length: Math.min(locations.length, Math.max(1, Math.trunc(maxConcurrent) || 1)) },
    async () => {
      while (next < locations.length) {
        if (signal?.aborted) return;
        const location = locations[next++];
        try {
          const result = await list(location.workspaceId, location.folderId);
          succeeded++;
          for (const session of result.sessions) {
            sessions.push({
              ...session,
              key: historySessionKey(location.workspaceId, location.folderId, session.number),
              workspaceId: location.workspaceId,
              workspaceName: location.workspaceName,
              folderId: location.folderId,
              folderName: location.folderName,
              folderPath: location.folderPath,
            });
          }
        } catch {
          if (signal?.aborted) return;
          // One unavailable workspace must not hide history from the others.
          failed.push(location);
        }
      }
    },
  );
  await Promise.all(workers);
  return { sessions: sortLocatedHistory(sessions), failed, succeeded };
}
