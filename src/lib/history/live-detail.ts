import type { HistorySessionDetail, HistorySessionSummary } from "../api/history";

/** Compare persisted checkpoint content, not transient tool/agent activity. */
export function historyDetailNeedsRefresh(
  current: HistorySessionDetail | null,
  latest: HistorySessionSummary,
): boolean {
  if (!current) return false;
  if (latest.revision) return latest.revision !== current.revision;
  // Graceful compatibility for older backends that do not expose content hashes.
  return latest.updatedAt !== current.updatedAt ||
    latest.status !== current.status ||
    latest.checkpointCount !== current.records.length ||
    latest.summary !== current.summary ||
    latest.title !== current.title;
}
