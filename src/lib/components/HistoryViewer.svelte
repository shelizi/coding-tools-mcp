<script lang="ts">
  import { onMount } from "svelte";
  import Clock3 from "@lucide/svelte/icons/clock-3";
  import FileText from "@lucide/svelte/icons/file-text";
  import History from "@lucide/svelte/icons/history";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import HistoryBackgroundJobs from "./HistoryBackgroundJobs.svelte";
  import {
    listHistorySessions,
    readHistorySession,
    type HistoryRecord,
    type HistorySessionDetail,
  } from "$lib/api/history";
  import { listWorkspaces } from "$lib/api/workspaces";
  import {
    collectHistory,
    historyLocationKey,
    uniqueHistoryLocations,
    type HistoryLocation,
    type LocatedHistorySession,
  } from "$lib/history/all-projects";
  import { historyDetailNeedsRefresh } from "$lib/history/live-detail";
  import { historyPollCadenceMs } from "$lib/history/polling";
  import { workspaceFolders, type WorkspaceFolder } from "$lib/types";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
    folders: WorkspaceFolder[];
    activeFolderId?: string;
  }

  let { workspaceId, folders, activeFolderId = "" }: Props = $props();
  let selectedScope = $state("all");
  let sources = $state<HistoryLocation[]>([]);
  let sessions = $state<LocatedHistorySession[]>([]);
  let selectedKey = $state<string | null>(null);
  let detail = $state<HistorySessionDetail | null>(null);
  let busy = $state(false);
  let detailBusy = $state(false);
  let detailSyncing = $state(false);
  let detailSyncError = $state("");
  let detailSyncedAt = $state<number | null>(null);
  let error = $state("");
  let failedCount = $state(0);
  let visibleCount = $state(60);
  let lastLoadKey = "";
  let showRaw = $state(false);
  let refreshSequence = 0;
  let detailSequence = 0;
  let detailRefreshInFlightFor: string | null = null;
  let activeRefreshes = 0;
  let activeRequest: AbortController | null = null;
  let lastPollAt = 0;

  const selectedSession = $derived(
    sessions.find((session) => session.key === selectedKey) ?? null,
  );
  const selectedSource = $derived(
    sources.find((source) => source.key === selectedScope) ?? null,
  );
  const displayedSessions = $derived(sessions.slice(0, visibleCount));
  // Checkpoints are appended chronologically to the archive; present the
  // newest first without rewriting their persisted Markdown order.
  const displayedRecords = $derived(detail ? [...detail.records].reverse() : []);

  function formatDate(value: string | null | undefined): string {
    if (!value) return $t("Unknown");
    if (value.startsWith("unix:")) {
      const seconds = Number(value.slice(5));
      if (Number.isFinite(seconds)) return new Date(seconds * 1000).toLocaleString();
    }
    const parsed = new Date(value);
    return Number.isNaN(parsed.valueOf()) ? value : parsed.toLocaleString();
  }

  function activityLabel(status: LocatedHistorySession["activityStatus"]): string {
    if (status === "running") return $t("Running now");
    if (status === "active") return $t("Active recently");
    if (status === "completed") return $t("Completed");
    return $t("Inactive");
  }

  function activityDotClass(status: LocatedHistorySession["activityStatus"]): string {
    if (status === "running") return "bg-[var(--color-success)] animate-pulse";
    if (status === "active") return "bg-[var(--color-accent)]";
    if (status === "completed") return "bg-[var(--color-success)]";
    return "bg-[var(--color-text-muted)]";
  }

  function sourceLabel(source: HistoryLocation): string {
    return source.workspaceName === source.folderName
      ? source.workspaceName
      : `${source.workspaceName} / ${source.folderName}`;
  }

  async function getLocations(reload: boolean): Promise<HistoryLocation[]> {
    if (!reload && sources.length > 0) return sources;
    const profiles = await listWorkspaces();
    return uniqueHistoryLocations(
      profiles.flatMap((profile) =>
        workspaceFolders(profile).map((folder) => ({
          key: historyLocationKey(profile.id, folder.id),
          workspaceId: profile.id,
          workspaceName: profile.name,
          folderId: folder.id,
          folderName: folder.name,
          folderPath: folder.path,
        })),
      ),
    );
  }

  async function refresh(
    scope = selectedScope,
    preserveSelection = false,
    silent = false,
  ) {
    if (!workspaceId || (silent && (busy || activeRefreshes > 0))) return;
    const requestId = ++refreshSequence;
    activeRequest?.abort();
    const request = new AbortController();
    activeRequest = request;
    activeRefreshes++;
    if (!silent) {
      busy = true;
      error = "";
    }
    try {
      const locations = await getLocations(!silent);
      if (requestId !== refreshSequence) return;
      sources = locations;
      // A workspace/folder might have been removed while the view was open.
      if (scope !== "all" && !locations.some((source) => source.key === scope)) {
        scope = "all";
        selectedScope = "all";
      }
      const targets = scope === "all"
        ? locations
        : locations.filter((source) => source.key === scope);
      const result = await collectHistory(
        targets,
        (id, folderId) => listHistorySessions(id, folderId, request.signal),
        4,
        request.signal,
      );
      if (requestId !== refreshSequence) return;
      if (targets.length > 0 && result.succeeded === 0) {
        throw new Error($t("Unable to load history sessions from any project."));
      }
      failedCount = result.failed.length;
      sessions = result.sessions;
      if (!preserveSelection || !sessions.some((item) => item.key === selectedKey)) {
        selectedKey = sessions[0]?.key ?? null;
        detail = null;
        detailSyncedAt = null;
        detailSyncError = "";
        detailSequence++;
        if (!silent) visibleCount = 60;
      }
      lastPollAt = Date.now();
      const current = sessions.find((item) => item.key === selectedKey);
      if (!silent) {
        if (current) await loadDetail(current);
      } else if (current && detail) {
        // Re-read persisted contents only after the revision changes.
        void refreshSelectedDetail(current);
      } else if (current && !detailBusy) {
        // Silent polls also recover when a selected session disappears.
        void loadDetail(current);
      }
    } catch (reason) {
      // A failed silent poll must not retry every 3 seconds across all projects.
      if (requestId === refreshSequence) lastPollAt = Date.now();
      if (!silent && requestId === refreshSequence) {
        error = String(reason);
        failedCount = 0;
        sessions = [];
        selectedKey = null;
        detail = null;
        detailSequence++;
      }
    } finally {
      activeRefreshes--;
      if (activeRequest === request) activeRequest = null;
      if (!silent && requestId === refreshSequence) busy = false;
    }
  }

  async function loadDetail(session: LocatedHistorySession) {
    const requestId = ++detailSequence;
    detailRefreshInFlightFor = null;
    detailSyncing = false;
    detailBusy = true;
    error = "";
    detailSyncError = "";
    try {
      const result = await readHistorySession(session.workspaceId, session.number, session.folderId);
      if (requestId !== detailSequence || session.key !== selectedKey) return;
      detail = result;
      showRaw = false;
      detailSyncedAt = Date.now();
    } catch (reason) {
      if (requestId !== detailSequence) return;
      error = String(reason);
      detail = null;
    } finally {
      if (requestId === detailSequence) detailBusy = false;
    }
  }

  async function refreshSelectedDetail(session: LocatedHistorySession) {
    if (!detail || !historyDetailNeedsRefresh(detail, session)) return;
    if (detailRefreshInFlightFor === session.key) return;
    const selectedSequence = detailSequence;
    detailRefreshInFlightFor = session.key;
    detailSyncing = true;
    try {
      const latest = await readHistorySession(session.workspaceId, session.number, session.folderId);
      if (selectedSequence !== detailSequence || session.key !== selectedKey) return;
      detail = latest;
      detailSyncedAt = Date.now();
      detailSyncError = "";
      // Leave raw-view and scroll untouched: existing turnId keys remain stable.
    } catch {
      if (selectedSequence === detailSequence && session.key === selectedKey) {
        detailSyncError = $t("Could not synchronize the latest session checkpoints.");
      }
    } finally {
      if (detailRefreshInFlightFor === session.key) {
        detailRefreshInFlightFor = null;
        detailSyncing = false;
      }
    }
  }

  async function selectScope(scope: string) {
    if (scope === selectedScope) return;
    selectedScope = scope;
    selectedKey = null;
    detail = null;
    detailSyncedAt = null;
    detailSyncError = "";
    failedCount = 0;
    detailSequence++;
    visibleCount = 60;
    await refresh(scope);
  }

  async function selectSession(session: LocatedHistorySession) {
    if (session.key === selectedKey && detail) return;
    selectedKey = session.key;
    detail = null;
    detailSyncedAt = null;
    detailSyncError = "";
    await loadDetail(session);
  }

  function recordHasContent(record: HistoryRecord): boolean {
    return Boolean(
      record.userIntent ||
        record.findings.length ||
        record.decisions.length ||
        record.filesChanged.length ||
        record.tests.length ||
        record.runtimeState.length ||
        record.remainingIssues.length ||
        record.nextActions.length ||
        record.notes,
    );
  }

  $effect(() => {
    const targetFolderId = activeFolderId || folders[0]?.id || "";
    const loadKey = JSON.stringify([workspaceId, targetFolderId]);
    if (!workspaceId || loadKey === lastLoadKey) return;
    lastLoadKey = loadKey;
    sources = [];
    selectedScope = "all";
    selectedKey = null;
    detail = null;
    detailSyncedAt = null;
    detailSyncError = "";
    detailSequence++;
    visibleCount = 60;
    void refresh("all");
  });

  onMount(() => {
    const interval = window.setInterval(() => {
      const cadence = historyPollCadenceMs(selectedScope, sources.length);
      if (document.hidden || Date.now() - lastPollAt < cadence) return;
      void refresh(selectedScope, true, true);
    }, 3_000);
    return () => {
      window.clearInterval(interval);
      activeRequest?.abort();
      refreshSequence++;
      detailSequence++;
    };
  });
</script>

<section class="tx-card p-5">
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div>
      <div class="flex items-center gap-2">
        <History size={18} class="text-[var(--color-accent)]" />
        <h3 class="font-semibold">{$t("History sessions")}</h3>
      </div>
      <p class="mt-1 max-w-2xl text-sm text-[var(--color-text-muted)]">
        {$t("Browse saved development sessions from all configured projects or filter by project folder.")}
      </p>
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <label class="sr-only" for="history-folder">{$t("History project filter")}</label>
      <select
        id="history-folder"
        class="tx-input max-w-64"
        value={selectedScope}
        onchange={(event) => void selectScope(event.currentTarget.value)}
      >
        <option value="all">{$t("All projects")}</option>
        {#each sources as source (source.key)}
          <option value={source.key}>{sourceLabel(source)}</option>
        {/each}
      </select>
      <button
        type="button"
        class="tx-btn-ghost"
        disabled={busy || !workspaceId}
        onclick={() => void refresh(selectedScope, true)}
      >
        <RefreshCw size={14} class={busy ? "inline-block animate-spin" : "inline-block"} />
        <span class="ml-1">{$t("Refresh")}</span>
      </button>
    </div>
  </div>

  {#if selectedScope === "all"}
    <p class="mt-3 text-xs text-[var(--color-text-muted)]">
      {$t("Showing history from {count} project folders", { count: sources.length })}
    </p>
  {:else if selectedSource}
    <p class="tx-mono mt-3 truncate text-xs text-[var(--color-text-muted)]" title={selectedSource.folderPath}>
      {selectedSource.folderPath} · docs/history-session
    </p>
  {/if}

  {#if error}
    <p class="mt-4 rounded-lg border border-[var(--color-error)]/30 bg-[var(--color-error)]/10 px-3 py-2 text-sm text-[var(--color-error)]">
      {error}
    </p>
  {/if}

  {#if failedCount > 0}
    <p class="mt-3 text-xs text-[var(--color-text-muted)]" role="status">
      {$t("History from {count} project folders could not be loaded.", { count: failedCount })}
    </p>
  {/if}

  {#if busy && sessions.length === 0}
    <p class="mt-5 text-sm text-[var(--color-text-muted)]" role="status">{$t("Loading history…")}</p>
  {:else if !error && sessions.length === 0}
    <div class="mt-5 rounded-lg border border-dashed border-[var(--color-border)] px-4 py-8 text-center">
      <FileText size={24} class="mx-auto text-[var(--color-text-muted)]" />
      <p class="mt-2 text-sm font-medium">{$t("No history sessions yet")}</p>
      <p class="mt-1 text-xs text-[var(--color-text-muted)]">
        {$t("A session appears here after ChatGPT completes its first history checkpoint.")}
      </p>
    </div>
  {:else if sessions.length > 0}
    <div class="mt-5 grid gap-4 lg:grid-cols-[minmax(13rem,0.34fr)_minmax(0,1fr)]">
      <nav aria-label={$t("History sessions")} class="space-y-2">
        {#each displayedSessions as session (session.key)}
          <button
            type="button"
            class={`w-full rounded-lg border px-3 py-3 text-left transition-colors ${
              selectedKey === session.key
                ? "border-[var(--color-accent)] bg-[var(--color-accent)]/10"
                : "border-[var(--color-border)] hover:border-[var(--color-accent)]/60"
            }`}
            aria-current={selectedKey === session.key ? "page" : undefined}
            onclick={() => void selectSession(session)}
          >
            <div class="flex items-start justify-between gap-2">
              <span class="min-w-0 truncate text-sm font-medium">{session.title}</span>
              <span class="inline-flex shrink-0 items-center gap-1.5 text-[11px] text-[var(--color-text-secondary)]">
                <span class={`h-2 w-2 rounded-full ${activityDotClass(session.activityStatus)}`}></span>
                {activityLabel(session.activityStatus)}
              </span>
            </div>
            <p class="mt-1 text-xs text-[var(--color-text-muted)]">
              #{session.number} · {formatDate(session.updatedAt)}
            </p>
            <p class="mt-1 truncate text-[11px] text-[var(--color-text-secondary)]" title={session.folderPath}>
              {session.workspaceName === session.folderName
                ? session.workspaceName
                : `${session.workspaceName} / ${session.folderName}`}
            </p>
            {#if session.activityDescription}
              <p class="tx-mono mt-2 truncate text-[11px] text-[var(--color-text-secondary)]" title={session.activityDescription}>
                {session.activityStatus === "running"
                  ? $t("Now: {action}", { action: session.activityDescription })
                  : $t("Last: {action}", { action: session.activityDescription })}
              </p>
            {/if}
            {#if session.lastActivityAtMs !== null}
              <p class="mt-1 text-[11px] text-[var(--color-text-muted)]">
                {$t("Last activity: {time}", { time: new Date(session.lastActivityAtMs).toLocaleString() })}
              </p>
            {/if}
            <p class="mt-2 line-clamp-2 text-xs leading-5 text-[var(--color-text-secondary)]">
              {session.summary}
            </p>
            <div class="mt-2 flex items-center gap-1 text-[11px] text-[var(--color-text-muted)]">
              <MessageSquare size={12} />
              {$t("{count} checkpoints", { count: session.checkpointCount })}
            </div>
          </button>
        {/each}
        {#if sessions.length > visibleCount}
          <button type="button" class="tx-btn-ghost w-full" onclick={() => (visibleCount += 60)}>
            {$t("Show more sessions")}
          </button>
        {/if}
      </nav>

      <div class="min-w-0">
        {#if detailBusy}
          <div class="rounded-lg border border-[var(--color-border)] px-4 py-10 text-center text-sm text-[var(--color-text-muted)]">
            {$t("Loading history…")}
          </div>
        {:else if detail}
          <article class="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)]">
            <header class="border-b border-[var(--color-border)] px-4 py-4">
              <div class="flex flex-wrap items-start justify-between gap-3">
                <div class="min-w-0">
                  <p class="tx-section-label">{$t("Session {number}", { number: detail.number })}</p>
                  {#if selectedSession}
                    <p class="mt-1 text-xs text-[var(--color-text-secondary)]" title={selectedSession.folderPath}>
                      {selectedSession.workspaceName === selectedSession.folderName
                        ? selectedSession.workspaceName
                        : `${selectedSession.workspaceName} / ${selectedSession.folderName}`}
                    </p>
                  {/if}
                  <h4 class="mt-1 truncate text-base font-semibold">{detail.title}</h4>
                  <p class="tx-mono mt-1 truncate text-xs text-[var(--color-text-muted)]" title={detail.path}>
                    {detail.path}
                  </p>
                </div>
                {#if selectedSession}
                  <span class="tx-status-pill cursor-default">
                    <span class={`h-2 w-2 rounded-full ${activityDotClass(selectedSession.activityStatus)}`}></span>
                    {activityLabel(selectedSession.activityStatus)}
                  </span>
                {:else}
                  <span class="tx-status-pill cursor-default">{detail.status}</span>
                {/if}
              </div>
              <div class="mt-3 flex flex-wrap gap-x-4 gap-y-1 text-xs text-[var(--color-text-muted)]">
                <span class="inline-flex items-center gap-1"><Clock3 size={12} /> {formatDate(detail.updatedAt)}</span>
                <span class="inline-flex items-center gap-1"><MessageSquare size={12} /> {$t("{count} checkpoints", { count: detail.records.length })}</span>
                <span class="inline-flex items-center gap-1">
                  <RefreshCw size={12} class={detailSyncing ? "animate-spin" : ""} />
                  {detailSyncing
                    ? $t("Synchronizing session updates…")
                    : $t("Session updates automatically")}
                  {#if detailSyncedAt !== null}
                    · {new Date(detailSyncedAt).toLocaleTimeString()}
                  {/if}
                </span>
              </div>
              {#if detailSyncError}
                <p class="mt-2 text-xs text-[var(--color-error)]" role="status">{detailSyncError}</p>
              {/if}
              {#if selectedSession?.activityDescription}
                <p class="tx-mono mt-3 truncate text-xs text-[var(--color-text-secondary)]" title={selectedSession.activityDescription}>
                  {selectedSession.activityStatus === "running"
                    ? $t("Now: {action}", { action: selectedSession.activityDescription })
                    : $t("Last: {action}", { action: selectedSession.activityDescription })}
                </p>
              {/if}
            </header>

            {#if selectedSession}
              {#key selectedSession.key}
                <HistoryBackgroundJobs
                  workspaceId={selectedSession.workspaceId}
                  folderId={selectedSession.folderId}
                  sessionNumber={selectedSession.number}
                  fallbackRecentActions={selectedSession.recentActions ?? []}
                />
              {/key}
            {/if}

            {#if detail.records.length > 0}
              <div class="divide-y divide-[var(--color-border)]">
                {#each displayedRecords as record (record.turnId)}
                  {#if recordHasContent(record)}
                    <section class="px-4 py-4">
                      <div class="flex flex-wrap items-center justify-between gap-2">
                        <h5 class="font-mono text-xs font-semibold text-[var(--color-text-secondary)]">{record.turnId}</h5>
                        <span class="text-xs text-[var(--color-text-muted)]">{formatDate(record.timestamp)}</span>
                      </div>
                      {#if record.userIntent}
                        <p class="mt-3 text-sm font-medium">{record.userIntent}</p>
                      {/if}
                      {#if record.findings.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Findings")}</p><ul class="mt-1 list-disc space-y-1 pl-5 text-sm text-[var(--color-text-secondary)]">{#each record.findings as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.decisions.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Decisions")}</p><ul class="mt-1 list-disc space-y-1 pl-5 text-sm text-[var(--color-text-secondary)]">{#each record.decisions as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.filesChanged.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Files changed")}</p><ul class="mt-1 list-disc space-y-1 pl-5 font-mono text-xs text-[var(--color-text-secondary)]">{#each record.filesChanged as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.tests.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Tests")}</p><ul class="mt-1 list-disc space-y-1 pl-5 text-sm text-[var(--color-text-secondary)]">{#each record.tests as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.runtimeState.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Runtime state")}</p><ul class="mt-1 list-disc space-y-1 pl-5 text-sm text-[var(--color-text-secondary)]">{#each record.runtimeState as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.remainingIssues.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Remaining issues")}</p><ul class="mt-1 list-disc space-y-1 pl-5 text-sm text-[var(--color-text-secondary)]">{#each record.remainingIssues as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.nextActions.length > 0}
                        <div class="mt-3"><p class="tx-section-label">{$t("Next actions")}</p><ul class="mt-1 list-disc space-y-1 pl-5 text-sm text-[var(--color-text-secondary)]">{#each record.nextActions as item}<li>{item}</li>{/each}</ul></div>
                      {/if}
                      {#if record.notes}
                        <p class="mt-3 whitespace-pre-wrap text-sm text-[var(--color-text-secondary)]">{record.notes}</p>
                      {/if}
                    </section>
                  {/if}
                {/each}
              </div>
            {:else}
              <p class="px-4 py-6 text-sm text-[var(--color-text-muted)]">{$t("No checkpoints recorded in this session.")}</p>
            {/if}

            <details class="border-t border-[var(--color-border)] px-4 py-3" bind:open={showRaw}>
              <summary class="cursor-pointer text-xs font-medium text-[var(--color-text-secondary)]">{$t("View raw Markdown record")}</summary>
              <pre class="mt-3 max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-md bg-[var(--color-panel)] p-3 font-mono text-xs leading-relaxed">{detail.content}</pre>
            </details>
          </article>
        {/if}
      </div>
    </div>
  {/if}
</section>
