import { getBackend } from "$lib/backend";

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
  /** SHA-256 of persisted history, independent of tool activity status. */
  revision?: string;
  recentActions?: HistoryRecentToolAction[];
  summary: string;
  checkpointCount: number;
}

export interface HistoryRecentToolAction {
  sequence: number;
  tool: string;
  action: string;
  startedAtMs: number;
  updatedAtMs: number;
  status: string;
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

export interface HistoryBackgroundJob {
  id: string;
  tool: string;
  action: string;
  status: "running" | "verifying";
  startedAtMs: number;
  elapsedMs: number;
}

export interface HistoryBackgroundJobsResult {
  jobs: HistoryBackgroundJob[];
  recentActions?: HistoryRecentToolAction[];
  capturedAtMs: number;
}

export interface HistoryBackgroundLog {
  jobId: string;
  status: string;
  stdout: string;
  stderr: string;
  stdoutTruncated: boolean;
  stderrTruncated: boolean;
  capturedAtMs: number;
}

export async function listHistorySessions(
  workspaceId: string,
  folderId?: string,
  signal?: AbortSignal,
): Promise<HistorySessionsResult> {
  return getBackend().history.list(workspaceId, folderId, signal);
}

export async function readHistorySession(
  workspaceId: string,
  number: number,
  folderId?: string,
): Promise<HistorySessionDetail> {
  return getBackend().history.read(workspaceId, number, folderId);
}

export async function listHistoryBackgroundJobs(
  workspaceId: string,
  number: number,
  folderId: string,
): Promise<HistoryBackgroundJobsResult> {
  return getBackend().history.listBackgroundJobs(workspaceId, number, folderId);
}

export async function readHistoryBackgroundLog(
  workspaceId: string,
  number: number,
  folderId: string,
  jobId: string,
): Promise<HistoryBackgroundLog> {
  return getBackend().history.readBackgroundLog(workspaceId, number, folderId, jobId);
}
