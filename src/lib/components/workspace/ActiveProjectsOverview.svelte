<script lang="ts">
  import { onMount } from "svelte";
  import Activity from "@lucide/svelte/icons/activity";
  import Clock3 from "@lucide/svelte/icons/clock-3";
  import FolderKanban from "@lucide/svelte/icons/folder-kanban";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import Terminal from "@lucide/svelte/icons/terminal";
  import {
    listHistoryBackgroundJobs,
    listHistorySessions,
    readHistoryBackgroundLog,
    type HistoryBackgroundJobsResult,
    type HistoryBackgroundLog,
  } from "$lib/api/history";
  import { listWorkspaces } from "$lib/api/workspaces";
  import {
    collectHistory,
    historyLocationKey,
    type HistoryLocation,
    type LocatedHistorySession,
  } from "$lib/history/all-projects";
  import {
    aggregateActiveProjects,
    type ActiveProjectJob,
  } from "$lib/history/active-projects";
  import { historyPollCadenceMs } from "$lib/history/polling";
  import { workspaceFolders } from "$lib/types";
  import { t } from "$lib/i18n";

  let sources = $state<HistoryLocation[]>([]);
  let sessions = $state<LocatedHistorySession[]>([]);
  let snapshots = $state<Record<string, HistoryBackgroundJobsResult>>({});
  let liveErrors = $state<Record<string, boolean>>({});
  const activeProjects = $derived(aggregateActiveProjects(sessions, snapshots));
  let busy = $state(true);
  let refreshing = $state(false);
  let error = $state("");
  let failedCount = $state(0);
  let checkedAt = $state<number | null>(null);
  let expandedJobs = $state<Record<string, boolean>>({});
  let selectedJobKey = $state<string | null>(null);
  const selectedJob = $derived(
    activeProjects.flatMap((project) => project.jobs).find((job) => job.key === selectedJobKey) ?? null,
  );
  let log = $state<HistoryBackgroundLog | null>(null);
  let logError = $state("");
  let logLoading = $state(false);
  let disposed = false;
  let historyInFlight = false;
  let liveInFlight = false;
  let logInFlightKey: string | null = null;
  let logGeneration = 0;
  let lastHistoryPollAt = 0;
  let lastSourcesAt = 0;

  function duration(milliseconds: number): string {
    const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);
    return hours ? `${hours}h ${minutes % 60}m` : minutes ? `${minutes}m ${seconds % 60}s` : `${seconds}s`;
  }

  function actionLabel(status: string): string {
    if (status === "running") return $t("Running now");
    if (status === "success") return $t("Succeeded");
    if (status === "failed" || status === "worker_failed") return $t("Failed");
    if (status === "cancelled") return $t("Cancelled");
    if (status === "observed") return $t("Previously observed");
    return $t("Completed");
  }

  async function refreshSources(): Promise<HistoryLocation[]> {
    // Workspace configurations are cheap, but scanning every history file is
    // not. Rescan the profile directory periodically, not every 3 seconds.
    if (sources.length && Date.now() - lastSourcesAt < 60_000) return sources;
    const profiles = await listWorkspaces();
    const locations = profiles.flatMap((profile) => workspaceFolders(profile).map((folder) => ({
        key: historyLocationKey(profile.id, folder.id),
        workspaceId: profile.id,
        workspaceName: profile.name,
        folderId: folder.id,
        folderName: folder.name,
        folderPath: folder.path,
      })));
    if (!disposed) {
      sources = locations;
      lastSourcesAt = Date.now();
    }
    return locations;
  }

  async function refreshHistory() {
    if (disposed || historyInFlight) return;
    historyInFlight = true;
    refreshing = true;
    try {
      const locations = await refreshSources();
      if (disposed) return;
      const result = await collectHistory(
        locations,
        (workspaceId, folderId) => listHistorySessions(workspaceId, folderId),
        4,
      );
      if (disposed) return;
      if (locations.length && result.succeeded === 0) {
        throw new Error($t("Unable to load active projects."));
      }
      // Only retain the two live categories. No idle/completed sessions,
      // document bodies, checkpoint summaries or conversation titles.
      const live = result.sessions.filter((item) =>
        item.activityStatus === "running" || item.activityStatus === "active" || item.activeRequestCount > 0);
      const activeKeys = new Set(live.map((item) => item.key));
      sessions = live;
      snapshots = Object.fromEntries(
        Object.entries(snapshots).filter(([key]) => activeKeys.has(key)),
      );
      liveErrors = Object.fromEntries(
        Object.entries(liveErrors).filter(([key]) => activeKeys.has(key)),
      );
      failedCount = result.failed.length;
      error = "";
      checkedAt = Date.now();
      void refreshLive();
    } catch {
      if (!disposed) {
        // Do not present a cached running state as live after every project
        // inventory request fails.
        error = $t("Unable to load active projects.");
        sessions = [];
        snapshots = {};
        liveErrors = {};
        failedCount = 0;
        checkedAt = null;
      }
    } finally {
      historyInFlight = false;
      lastHistoryPollAt = Date.now();
      if (!disposed) {
        refreshing = false;
        busy = false;
      }
    }
  }

  async function refreshLive() {
    if (disposed || liveInFlight || !sessions.length) return;
    liveInFlight = true;
    const pending = sessions.filter((item) =>
      item.activityStatus === "running" || item.activityStatus === "active" || item.activeRequestCount > 0);
    const next: Record<string, HistoryBackgroundJobsResult> = {};
    const errors: Record<string, boolean> = {};
    let cursor = 0;
    try {
      await Promise.all(Array.from({ length: Math.min(4, pending.length) }, async () => {
        while (cursor < pending.length && !disposed) {
          const session = pending[cursor++];
          try {
            next[session.key] = await listHistoryBackgroundJobs(
              session.workspaceId, session.number, session.folderId,
            );
          } catch {
            // One archived session disappearing must not hide other projects.
            // Never leave a stale background job labelled as still running
            // after a failed RPC. Keep only the last known tool timeline.
            errors[session.key] = true;
            next[session.key] = {
              jobs: [],
              recentActions: snapshots[session.key]?.recentActions ?? session.recentActions ?? [],
              capturedAtMs: Date.now(),
            };
          }
        }
      }));
      if (disposed) return;
      const currentKeys = new Set(sessions.map((item) => item.key));
      snapshots = {
        ...Object.fromEntries(
          Object.entries(snapshots).filter(([key]) => currentKeys.has(key)),
        ),
        ...Object.fromEntries(Object.entries(next).filter(([key]) => currentKeys.has(key))),
      };
      liveErrors = Object.fromEntries(
        Object.entries(errors).filter(([key]) => currentKeys.has(key)),
      );
    } finally {
      liveInFlight = false;
    }
  }

  async function refreshLog(job: ActiveProjectJob) {
    if (disposed || logInFlightKey === job.key) return;
    const generation = logGeneration;
    logInFlightKey = job.key;
    logLoading = !log;
    try {
      const result = await readHistoryBackgroundLog(
        job.session.workspaceId,
        job.session.number,
        job.session.folderId,
        job.id,
      );
      if (disposed || selectedJobKey !== job.key || generation !== logGeneration) return;
      log = result;
      logError = "";
    } catch {
      if (!disposed && selectedJobKey === job.key && generation === logGeneration) {
        logError = $t("Unable to read this background job's output.");
      }
    } finally {
      if (logInFlightKey === job.key) logInFlightKey = null;
      if (!disposed && selectedJobKey === job.key && generation === logGeneration) logLoading = false;
    }
  }

  function toggleJob(job: ActiveProjectJob) {
    selectedJobKey = selectedJobKey === job.key ? null : job.key;
    log = null;
    logError = "";
    logGeneration++;
    if (selectedJobKey) void refreshLog(job);
  }

  function refreshNow() {
    void refreshHistory();
    void refreshLive();
    if (selectedJob) void refreshLog(selectedJob);
  }

  onMount(() => {
    disposed = false;
    void refreshHistory();
    const interval = window.setInterval(() => {
      if (disposed || document.hidden) return;
      if (Date.now() - lastHistoryPollAt >= historyPollCadenceMs("all", sources.length)) {
        void refreshHistory();
      }
      void refreshLive();
      if (selectedJob) void refreshLog(selectedJob);
    }, 3_000);
    return () => {
      disposed = true;
      logGeneration++;
      window.clearInterval(interval);
    };
  });
</script>

<section class="tx-card mb-3 p-3" aria-label={$t("Active project activity")}>
  <div class="flex items-center justify-between gap-2">
    <div>
      <div class="flex items-center gap-2">
        <Activity size={17} class="text-[var(--color-accent)]" />
        <h3 class="text-sm font-semibold">{$t("Active project activity")}</h3>
        {#if !busy}<span class="text-xs text-[var(--color-text-muted)]">({activeProjects.length})</span>{/if}
      </div>
    </div>
    <button type="button" class="tx-btn-ghost p-1" onclick={refreshNow} title={$t("Refresh")} aria-label={$t("Refresh")}>
      <RefreshCw size={14} class={refreshing ? "inline-block animate-spin" : "inline-block"} />
    </button>
  </div>
  {#if error}
    <p class="mt-3 text-xs text-[var(--color-error)]" role="status">{error}</p>
  {/if}
  {#if failedCount > 0}
    <p class="mt-2 text-xs text-[var(--color-text-muted)]">
      {$t("History from {count} project folders could not be loaded.", { count: failedCount })}
    </p>
  {/if}
  {#if busy}
    <p class="mt-4 text-sm text-[var(--color-text-muted)]">{$t("Loading activity…")}</p>
  {:else if activeProjects.length === 0 && !error}
    <p class="mt-4 text-sm text-[var(--color-text-muted)]">
      {$t("No projects are running or recently active.")}
    </p>
  {:else}
    <div class="mt-2 grid gap-2 xl:grid-cols-2">
      {#each activeProjects as project (project.key)}
        <article class="min-w-0 rounded-lg border border-[var(--color-border)] px-3 py-2">
          <div class="flex items-center justify-between gap-2">
            <div class="flex min-w-0 flex-1 items-center gap-2">
              <FolderKanban size={17} class="shrink-0 text-[var(--color-text-muted)]" />
              <div class="min-w-0">
                <h4 class="truncate text-xs font-semibold" title={project.folderPath}>{project.folderName}</h4>
              </div>
            </div>
            <span class="inline-flex shrink-0 items-center gap-1.5 text-xs text-[var(--color-text-secondary)]">
              <span class={project.status === "running"
                ? "h-2 w-2 animate-pulse rounded-full bg-[var(--color-success)]"
                : "h-2 w-2 rounded-full bg-[var(--color-accent)]"}></span>
              {project.status === "running" ? $t("Running now") : $t("Active recently")}
            </span>
          </div>

          <div class="mt-2 grid gap-2 sm:grid-cols-2">
            <section class="min-w-0">
              <h5 class="text-xs font-semibold">{$t("Live session activity")}</h5>
              {#if project.actions.length}
                <div class="mt-1 space-y-1">
                  {#each project.actions.slice(0, 2) as action (action.key)}
                    <div class="min-w-0">
                      <p class="tx-mono truncate text-[11px]" title={action.action || action.tool}>
                        {action.action || action.tool}
                      </p>
                      <p class="text-[11px] text-[var(--color-text-muted)]">{actionLabel(action.status)}</p>
                    </div>
                  {/each}
                </div>
              {:else if project.activityDescription}
                <p class="mt-2 text-xs text-[var(--color-text-secondary)]">
                  {project.activityDescription}
                </p>
              {:else}
                <p class="mt-2 text-xs text-[var(--color-text-muted)]">
                  {$t("No recent tool activity.")}
                </p>
              {/if}
            </section>

            <section class="min-w-0">
              <div class="flex items-center gap-2">
                <Terminal size={14} class="text-[var(--color-text-muted)]" />
                <h5 class="text-xs font-semibold">{$t("Background jobs")}</h5>
                {#if project.jobs.length}
                  <span class="text-[11px] text-[var(--color-text-muted)]">({project.jobs.length})</span>
                {/if}
              </div>
              {#if !project.jobs.length}
                <p class={project.sessions.some((session) => liveErrors[session.key])
                  ? "mt-2 text-xs text-[var(--color-error)]"
                  : "mt-2 text-xs text-[var(--color-text-muted)]"}>
                  {project.sessions.some((session) => liveErrors[session.key])
                    ? $t("Unable to refresh background jobs.")
                    : $t("No background jobs running.")}
                </p>
              {:else}
                <div class="mt-1 space-y-1">
                  {#each (expandedJobs[project.key] ? project.jobs : project.jobs.slice(0, 2)) as job (job.key)}
                    <button
                      type="button"
                      title={$t("Click to view live output")}
                      class="w-full rounded border border-[var(--color-border)] px-2 py-1 text-left text-[11px] hover:border-[var(--color-accent)]"
                      aria-expanded={selectedJobKey === job.key}
                      onclick={() => toggleJob(job)}
                    >
                      <span class="flex items-center justify-between gap-2">
                        <span class="tx-mono min-w-0 truncate" title={job.action || job.tool}>{job.action || job.tool}</span>
                        <span class="shrink-0 text-[var(--color-text-muted)]">
                          {job.status === "verifying" ? $t("Verifying") : duration(job.elapsedMs)}
                        </span>
                      </span>
                    </button>
                  {/each}
                  {#if project.jobs.length > 2}
                    <button type="button" class="tx-btn-ghost text-xs"
                      onclick={() => (expandedJobs[project.key] = !expandedJobs[project.key])}>
                      {expandedJobs[project.key] ? $t("Show fewer background jobs") : $t("Show all background jobs")}
                    </button>
                  {/if}
                </div>
              {/if}
            </section>
          </div>

          {#if selectedJob && project.jobs.some((job) => job.key === selectedJobKey)}
            <div class="mt-2 min-w-0 rounded-lg border border-[var(--color-border)]">
              <div class="flex items-center justify-between gap-3 border-b border-[var(--color-border)] px-3 py-2">
                <p class="tx-mono min-w-0 truncate text-xs" title={selectedJob.action || selectedJob.tool}>
                  {selectedJob.action || selectedJob.tool}
                </p>
                <button type="button" class="tx-btn-ghost shrink-0 text-xs"
                  onclick={() => toggleJob(selectedJob)}>{$t("Close log")}</button>
              </div>
              {#if logError}<p class="p-3 text-xs text-[var(--color-error)]" role="status">{logError}</p>{/if}
              {#if logLoading && !log}
                <p class="p-3 text-xs text-[var(--color-text-muted)]">{$t("Loading output…")}</p>
              {:else if log}
                <div class="space-y-3 p-3">
                  <div>
                    <p class="tx-section-label">stdout</p>
                    <pre class="mt-1 max-h-64 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--color-bg-secondary)] p-3 font-mono text-xs text-[var(--color-text-secondary)]">{log.stdout || $t("No output yet.")}</pre>
                  </div>
                  <div>
                    <p class="tx-section-label">stderr</p>
                    <pre class="mt-1 max-h-40 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--color-bg-secondary)] p-3 font-mono text-xs text-[var(--color-text-secondary)]">{log.stderr || $t("No output yet.")}</pre>
                  </div>
                  {#if log.stdoutTruncated || log.stderrTruncated}
                    <p class="text-xs text-[var(--color-text-muted)]">{$t("Showing only the latest retained output.")}</p>
                  {/if}
                  <p class="text-[11px] text-[var(--color-text-muted)]">
                    <Clock3 size={11} class="inline-block" />
                    {$t("Updated: {time}", { time: new Date(log.capturedAtMs).toLocaleTimeString() })}
                  </p>
                </div>
              {/if}
            </div>
          {/if}
        </article>
      {/each}
    </div>
  {/if}
  {#if checkedAt !== null && !busy}
    <p class="mt-3 text-right text-[11px] text-[var(--color-text-muted)]">
      {$t("Updated: {time}", { time: new Date(checkedAt).toLocaleTimeString() })}
    </p>
  {/if}
</section>
