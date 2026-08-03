import { invoke } from "@tauri-apps/api/core";

export interface HistorySessionSummary {
  number: number;
  path: string;
  title: string;
  sessionKey: string | null;
  createdAt: string | null;
  updatedAt: string | null;
  status: string;
  activityStatus: "running" | "active" | "inactive" | "completed";
  activityTool: string | null;
  activityDescription: string | null;
  lastActivityAtMs: number | null;
  activeRequestCount: number;
  lastActivityOutcome: string | null;
  summary: string;
  checkpointCount: number;
}

export interface HistoryRecord {
  turnId: string;
  timestamp: string;
  userIntent: string;
  findings: string[];
  decisions: string[];
  filesChanged: string[];
  tests: string[];
  runtimeState: string[];
  remainingIssues: string[];
  nextActions: string[];
  notes: string;
}

export interface HistorySessionsResult {
  historyDir: string;
  sessions: HistorySessionSummary[];
  count: number;
  missingNumbers: number[];
  invalidFiles: string[];
  emptyFiles: string[];
}

export interface HistorySessionDetail extends HistorySessionSummary {
  records: HistoryRecord[];
  content: string;
}

export async function listHistorySessions(
  workspaceId: string,
  folderId?: string,
): Promise<HistorySessionsResult> {
  return invoke<HistorySessionsResult>("list_history_sessions", {
    id: workspaceId,
    folderId,
  });
}

export async function readHistorySession(
  workspaceId: string,
  number: number,
  folderId?: string,
): Promise<HistorySessionDetail> {
  return invoke<HistorySessionDetail>("read_history_session", {
    id: workspaceId,
    number,
    folderId,
  });
}
