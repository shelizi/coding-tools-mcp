<script lang="ts">
  import { onMount } from "svelte";
  import Clock3 from "@lucide/svelte/icons/clock-3";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import Terminal from "@lucide/svelte/icons/terminal";
  import {
    listHistoryBackgroundJobs,
    readHistoryBackgroundLog,
    type HistoryBackgroundJob,
    type HistoryBackgroundLog,
    type HistoryRecentToolAction,
  } from "$lib/api/history";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
    folderId: string;
    sessionNumber: number;
    fallbackRecentActions?: HistoryRecentToolAction[];
  }

  let { workspaceId, folderId, sessionNumber, fallbackRecentActions = [] }: Props = $props();
  let jobs = $state<HistoryBackgroundJob[]>([]);
  let liveRecentActions = $state<HistoryRecentToolAction[]>([]);
  // A runtime restart can clear the 3s live endpoint's in-memory timeline.
  // Keep the recent, verified Markdown observations from the History list.
  const recentActions = $derived(liveRecentActions.length ? liveRecentActions : fallbackRecentActions);
  let showMoreActions = $state(false);
  const visibleActions = $derived(recentActions.slice(0, showMoreActions ? 16 : 6));
  let selectedJobId = $state<string | null>(null);
  let selectedJobLabel = $state("");
  let log = $state<HistoryBackgroundLog | null>(null);
  let loadingJobs = $state(true);
  let loadingLog = $state(false);
  let jobsError = $state("");
  let logError = $state("");
  let disposed = false;
  let jobsInFlight = false;
  let logInFlight = false;
  let logGeneration = 0;

  function duration(milliseconds: number): string {
    const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
    const minutes = Math.floor(seconds / 60);
    const hours = Math.floor(minutes / 60);
    return hours ? `${hours}h ${minutes % 60}m` : minutes ? `${minutes}m ${seconds % 60}s` : `${seconds}s`;
  }

  function actionStatus(status: string): string {
    if (status === "running") return $t("Running now");
    if (status === "success") return $t("Succeeded");
    if (status === "failed") return $t("Failed");
    if (status === "cancelled") return $t("Cancelled");
    if (status === "observed") return $t("Previously observed");
    return $t("Completed");
  }

  async function refreshJobs() {
    if (disposed || jobsInFlight) return;
    jobsInFlight = true;
    try {
      const result = await listHistoryBackgroundJobs(workspaceId, sessionNumber, folderId);
      if (disposed) return;
      jobs = result.jobs;
      liveRecentActions = result.recentActions ?? [];
      jobsError = "";
    } catch {
      if (!disposed) jobsError = $t("Unable to refresh background jobs.");
    } finally {
      jobsInFlight = false;
      if (!disposed) loadingJobs = false;
    }
  }

  async function refreshLog(jobId: string) {
    if (disposed || logInFlight) return;
    const generation = ++logGeneration;
    logInFlight = true;
    loadingLog = log === null;
    try {
      const result = await readHistoryBackgroundLog(workspaceId, sessionNumber, folderId, jobId);
      if (disposed || selectedJobId !== jobId || generation !== logGeneration) return;
      log = result;
      logError = "";
    } catch {
      if (!disposed && selectedJobId === jobId && generation === logGeneration) {
        logError = $t("Unable to read this background job's output.");
      }
    } finally {
      logInFlight = false;
      if (!disposed) loadingLog = false;
    }
  }

  function selectJob(job: HistoryBackgroundJob) {
    if (selectedJobId === job.id) {
      selectedJobId = null;
      selectedJobLabel = "";
      log = null;
      logError = "";
      logGeneration++;
      return;
    }
    selectedJobId = job.id;
    selectedJobLabel = job.action || job.tool;
    log = null;
    logError = "";
    logGeneration++;
    void refreshLog(job.id);
  }

  onMount(() => {
    void refreshJobs();
    const interval = window.setInterval(() => {
      if (document.hidden || disposed) return;
      void refreshJobs();
      if (selectedJobId) void refreshLog(selectedJobId);
    }, 3_000);
    return () => {
      disposed = true;
      logGeneration++;
      window.clearInterval(interval);
    };
  });
</script>

<section class="border-b border-[var(--color-border)] px-4 py-4" aria-label={$t("Background jobs")}>
  <div class="mb-4 border-b border-[var(--color-border)] pb-4">
    <div class="flex items-center justify-between gap-3">
      <h5 class="text-sm font-semibold">{$t("Live session activity")}</h5>
      <span class="text-xs text-[var(--color-text-muted)]">{$t("Automatically refreshed")}</span>
    </div>
    <p class="mt-1 text-xs text-[var(--color-text-muted)]">
      {$t("Observed MCP tool operations, not an AI-generated progress summary.")}
    </p>
    {#if loadingJobs}
      <p class="mt-2 text-xs text-[var(--color-text-muted)]">{$t("Loading activity…")}</p>
    {:else if recentActions.length === 0}
      <p class="mt-2 text-xs text-[var(--color-text-muted)]">{$t("No recent tool activity recorded in this session.")}</p>
    {:else}
      <div class="mt-3 space-y-2">
        {#each visibleActions as activity (activity.sequence)}
          <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 text-xs">
            <span class="tx-mono min-w-0 max-w-full truncate text-[var(--color-text-secondary)]" title={activity.action}>
              {activity.action || activity.tool}
            </span>
            <span class="flex shrink-0 items-center gap-2 text-[11px] text-[var(--color-text-muted)]">
              <span>{new Date(activity.updatedAtMs).toLocaleTimeString()}</span>
              <span class={activity.status === "failed" ? "text-[var(--color-error)]" : activity.status === "running" ? "text-[var(--color-success)]" : ""}>
                {actionStatus(activity.status)}
              </span>
            </span>
          </div>
        {/each}
      </div>
      {#if recentActions.length > 6}
        <button
          type="button"
          class="tx-btn-ghost mt-2 text-xs"
          onclick={() => (showMoreActions = !showMoreActions)}
        >
          {showMoreActions ? $t("Show less activity") : $t("Show more activity")}
        </button>
      {/if}
    {/if}
  </div>
  <div class="flex items-center justify-between gap-3">
    <div class="flex items-center gap-2">
      <Terminal size={15} class="text-[var(--color-accent)]" />
      <h5 class="text-sm font-semibold">{$t("Background jobs")}</h5>
      {#if jobs.length > 0}
        <span class="text-xs text-[var(--color-text-muted)]">({jobs.length})</span>
      {/if}
    </div>
    <button
      class="tx-btn-ghost text-xs"
      type="button"
      title={$t("Refresh")}
      onclick={() => { void refreshJobs(); if (selectedJobId) void refreshLog(selectedJobId); }}
    >
      <RefreshCw size={13} class="inline-block" />
      {$t("Refresh")}
    </button>
  </div>

  {#if jobsError}
    <p class="mt-2 text-xs text-[var(--color-error)]" role="status">{jobsError}</p>
  {/if}
  {#if loadingJobs}
    <p class="mt-2 text-xs text-[var(--color-text-muted)]">{$t("Loading background jobs…")}</p>
  {:else if jobs.length === 0}
    <p class="mt-2 text-xs text-[var(--color-text-muted)]">{$t("No background jobs running in this session.")}</p>
  {:else}
    <div class="mt-3 space-y-2">
      {#each jobs as job (job.id)}
        <button
          type="button"
          class="w-full rounded-lg border border-[var(--color-border)] px-3 py-2 text-left transition-colors hover:border-[var(--color-accent)]/60"
          aria-expanded={selectedJobId === job.id}
          onclick={() => selectJob(job)}
        >
          <span class="flex items-center justify-between gap-2">
            <span class="flex min-w-0 items-center gap-2">
              <span class="h-2 w-2 shrink-0 animate-pulse rounded-full bg-[var(--color-success)]"></span>
              <span class="tx-mono truncate text-xs font-medium">{job.action || job.tool}</span>
            </span>
            <span class="shrink-0 text-[11px] text-[var(--color-text-muted)]">
              {job.status === "verifying" ? $t("Verifying") : $t("Running now")}
            </span>
          </span>
          <span class="mt-1 flex items-center gap-1 text-[11px] text-[var(--color-text-muted)]">
            <Clock3 size={11} /> {duration(job.elapsedMs)}
            <span>· {$t("Click to view live output")}</span>
          </span>
        </button>
      {/each}
    </div>
  {/if}

  {#if selectedJobId}
    <div class="mt-3 min-w-0 rounded-lg border border-[var(--color-border)]">
      <div class="flex items-center justify-between gap-2 border-b border-[var(--color-border)] px-3 py-2">
        <p class="tx-mono min-w-0 truncate text-xs" title={selectedJobLabel}>{selectedJobLabel}</p>
        <button
          type="button"
          class="tx-btn-ghost shrink-0 text-xs"
          onclick={() => {
            selectedJobId = null;
            selectedJobLabel = "";
            log = null;
            logError = "";
            logGeneration++;
          }}
        >{$t("Close log")}</button>
      </div>
      {#if logError}
        <p class="px-3 py-2 text-xs text-[var(--color-error)]" role="status">{logError}</p>
      {/if}
      {#if loadingLog && !log}
        <p class="px-3 py-3 text-xs text-[var(--color-text-muted)]">{$t("Loading output…")}</p>
      {:else if log}
        <div class="space-y-3 px-3 py-3">
          <div>
            <p class="tx-section-label">stdout</p>
            <pre class="mt-1 max-h-72 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--color-bg-secondary)] p-3 font-mono text-xs text-[var(--color-text-secondary)]">{log.stdout || $t("No output yet.")}</pre>
          </div>
          <div>
            <p class="tx-section-label">stderr</p>
            <pre class="mt-1 max-h-48 overflow-auto whitespace-pre-wrap break-all rounded-md bg-[var(--color-bg-secondary)] p-3 font-mono text-xs text-[var(--color-text-secondary)]">{log.stderr || $t("No output yet.")}</pre>
          </div>
          {#if log.stdoutTruncated || log.stderrTruncated}
            <p class="text-xs text-[var(--color-text-muted)]">{$t("Showing only the latest retained output.")}</p>
          {/if}
          <p class="text-[11px] text-[var(--color-text-muted)]">
            {$t("Updated: {time}", { time: new Date(log.capturedAtMs).toLocaleTimeString() })}
            · {log.status === "running" ? $t("Running now") : log.status === "verifying" ? $t("Verifying") : $t("Completed")}
          </p>
        </div>
      {/if}
    </div>
  {/if}
</section>
