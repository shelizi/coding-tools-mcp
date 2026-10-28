import type {
  HistoryBackgroundJob,
  HistoryBackgroundJobsResult,
  HistoryRecentToolAction,
} from "../api/history";
import type { LocatedHistorySession } from "./all-projects";

export interface ActiveProjectAction extends HistoryRecentToolAction {
  key: string;
}

export interface ActiveProjectJob extends HistoryBackgroundJob {
  key: string;
  session: LocatedHistorySession;
}

export interface ActiveHistoryProject {
  key: string;
  workspaceId: string;
  workspaceName: string;
  folderId: string;
  folderName: string;
  folderPath: string;
  status: "running" | "active";
  lastActivityAtMs: number | null;
  activityDescription: string | null;
  sessions: LocatedHistorySession[];
  actions: ActiveProjectAction[];
  jobs: ActiveProjectJob[];
}

function physicalProjectKey(session: LocatedHistorySession): string {
  // A project can be registered under multiple workspace profiles. Keep one
  // project card while preserving each runtime's own authenticated session
  // identity for log reads.
  const normalized = session.folderPath.replace(/\\/g, "/").replace(/\/+$/, "");
  const path = /^[a-z]:\//i.test(normalized) || normalized.startsWith("//")
    ? normalized.toLowerCase() : normalized;
  return path
    ? JSON.stringify(["path", path])
    : JSON.stringify(["folder", session.workspaceId, session.folderId]);
}

/**
 * Overview is about projects, not archived conversations. Never display the
 * checkpoint text or the session title. A project is visible only while one
 * of its sessions is genuinely running or recently active according to the
 * same backend activity tracker used by History.
 *
 * Live snapshots from the lightweight jobs endpoint override the slower
 * history scan's tool timeline without triggering a Markdown detail read.
 */
export function aggregateActiveProjects(
  sessions: readonly LocatedHistorySession[],
  snapshots: Readonly<Record<string, HistoryBackgroundJobsResult>> = {},
  maxActions = 6,
): ActiveHistoryProject[] {
  const projects = new Map<string, ActiveHistoryProject>();
  for (const session of sessions) {
    // A semantic archive can say "completed" while its managed command is
    // still running. Actual in-flight work wins for the project overview.
    const running = session.activityStatus === "running" || session.activeRequestCount > 0;
    if (!running && session.activityStatus !== "active") continue;
    const key = physicalProjectKey(session);
    let project = projects.get(key);
    if (!project) {
      project = {
        key,
        workspaceId: session.workspaceId,
        workspaceName: session.workspaceName,
        folderId: session.folderId,
        folderName: session.folderName,
        folderPath: session.folderPath,
        status: running ? "running" : "active",
        lastActivityAtMs: session.lastActivityAtMs,
        activityDescription: session.activityDescription,
        sessions: [],
        actions: [],
        jobs: [],
      };
      projects.set(key, project);
    }
    project.sessions.push(session);
    if (running) project.status = "running";
    const lastAt = session.lastActivityAtMs ??
      (session.updatedAt?.startsWith("unix:")
        ? Number(session.updatedAt.slice(5)) * 1_000 || 0
        : Date.parse(session.updatedAt ?? "") || 0);
    if (lastAt > (project.lastActivityAtMs ?? 0)) {
      project.lastActivityAtMs = lastAt;
      project.activityDescription = session.activityDescription;
    }

    const snapshot = snapshots[session.key];
    for (const action of snapshot?.recentActions?.length
      ? snapshot.recentActions : session.recentActions ?? []) {
      if (action.updatedAtMs > (project.lastActivityAtMs ?? 0)) {
        project.lastActivityAtMs = action.updatedAtMs;
      }
      project.actions.push({
        ...action,
        key: JSON.stringify([session.key, action.sequence]),
      });
    }
    for (const job of snapshot?.jobs ?? []) {
      if (job.startedAtMs > (project.lastActivityAtMs ?? 0)) {
        project.lastActivityAtMs = job.startedAtMs;
      }
      project.jobs.push({
        ...job,
        key: JSON.stringify([session.key, job.id]),
        session,
      });
    }
  }

  return [...projects.values()].map((project) => {
    project.actions.sort((a, b) =>
      b.updatedAtMs - a.updatedAtMs || b.sequence - a.sequence || a.key.localeCompare(b.key));
    project.actions = project.actions.slice(0, Math.max(0, maxActions));
    project.jobs.sort((a, b) => b.startedAtMs - a.startedAtMs || a.key.localeCompare(b.key));
    if (project.jobs.length) project.status = "running";
    return project;
  }).sort((a, b) =>
    (a.status === "running" ? 0 : 1) - (b.status === "running" ? 0 : 1) ||
    (b.lastActivityAtMs ?? 0) - (a.lastActivityAtMs ?? 0) ||
    a.key.localeCompare(b.key));
}
