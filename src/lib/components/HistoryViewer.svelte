<script lang="ts">
  import { onMount } from "svelte";
  import Clock3 from "@lucide/svelte/icons/clock-3";
  import FileText from "@lucide/svelte/icons/file-text";
  import History from "@lucide/svelte/icons/history";
  import MessageSquare from "@lucide/svelte/icons/message-square";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import {
    listHistorySessions,
    readHistorySession,
    type HistoryRecord,
    type HistorySessionDetail,
    type HistorySessionSummary,
  } from "$lib/api/history";
  import type { WorkspaceFolder } from "$lib/types";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
    folders: WorkspaceFolder[];
    activeFolderId?: string;
  }

  let { workspaceId, folders, activeFolderId = "" }: Props = $props();
  let selectedFolderId = $state("");
  let sessions = $state<HistorySessionSummary[]>([]);
  let selectedNumber = $state<number | null>(null);
  let detail = $state<HistorySessionDetail | null>(null);
  let busy = $state(false);
  let detailBusy = $state(false);
  let error = $state("");
  let lastLoadKey = "";
  let showRaw = $state(false);

  const selectedSession = $derived(
    sessions.find((session) => session.number === selectedNumber) ?? null,
  );

  const selectedFolder = $derived(
    folders.find((folder) => folder.id === selectedFolderId) ?? folders[0],
  );

  function formatDate(value: string | null | undefined): string {
    if (!value) return $t("Unknown");
    if (value.startsWith("unix:")) {
      const seconds = Number(value.slice(5));
      if (Number.isFinite(seconds)) return new Date(seconds * 1000).toLocaleString();
    }
    const parsed = new Date(value);
    return Number.isNaN(parsed.valueOf()) ? value : parsed.toLocaleString();
  }

  function sessionTimestamp(session: HistorySessionSummary): number {
    if (session.lastActivityAtMs !== null) return session.lastActivityAtMs;
    if (!session.updatedAt) return 0;
    if (session.updatedAt.startsWith("unix:")) {
      return Number(session.updatedAt.slice(5)) * 1000 || 0;
    }
    return Date.parse(session.updatedAt) || 0;
  }

  function sortSessions(items: HistorySessionSummary[]): HistorySessionSummary[] {
    const priority = { running: 0, active: 1, inactive: 2, completed: 3 } as const;
    return [...items].sort((left, right) => {
      const statusOrder = priority[left.activityStatus] - priority[right.activityStatus];
      return statusOrder || sessionTimestamp(right) - sessionTimestamp(left) || right.number - left.number;
    });
  }

  function activityLabel(status: HistorySessionSummary["activityStatus"]): string {
    if (status === "running") return $t("Running now");
    if (status === "active") return $t("Active recently");
    if (status === "completed") return $t("Completed");
    return $t("Inactive");
  }

  function activityDotClass(status: HistorySessionSummary["activityStatus"]): string {
    if (status === "running") return "bg-[var(--color-success)] animate-pulse";
    if (status === "active") return "bg-[var(--color-accent)]";
    if (status === "completed") return "bg-[var(--color-success)]";
    return "bg-[var(--color-text-muted)]";
  }

  async function refresh(
    folderId = selectedFolderId,
    preserveSelection = false,
    silent = false,
  ) {
    if (!workspaceId || !folderId || busy) return;
    if (!silent) {
      busy = true;
      error = "";
    }
    try {
      const result = await listHistorySessions(workspaceId, folderId);
      sessions = sortSessions(result.sessions);
      if (!preserveSelection || !sessions.some((item) => item.number === selectedNumber)) {
        selectedNumber = sessions[0]?.number ?? null;
        detail = null;
      }
      if (!silent && selectedNumber !== null) {
        await loadDetail(selectedNumber, folderId);
      }
    } catch (reason) {
      if (!silent) {
        error = String(reason);
        sessions = [];
        selectedNumber = null;
        detail = null;
      }
    } finally {
      if (!silent) busy = false;
    }
  }

  async function loadDetail(number: number, folderId = selectedFolderId) {
    if (!workspaceId || !folderId) return;
    detailBusy = true;
    error = "";
    try {
      detail = await readHistorySession(workspaceId, number, folderId);
      selectedNumber = number;
      showRaw = false;
    } catch (reason) {
      error = String(reason);
      detail = null;
    } finally {
      detailBusy = false;
    }
  }

  async function selectFolder(folderId: string) {
    if (!folderId || folderId === selectedFolderId) return;
    selectedFolderId = folderId;
    selectedNumber = null;
    detail = null;
    await refresh(folderId);
  }

  async function selectSession(number: number) {
    if (number === selectedNumber && detail) return;
    selectedNumber = number;
    await loadDetail(number);
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
    const loadKey = `${workspaceId}:${targetFolderId}`;
    if (!workspaceId || !targetFolderId || loadKey === lastLoadKey) return;
    lastLoadKey = loadKey;
    selectedFolderId = targetFolderId;
    void refresh(targetFolderId);
  });

  onMount(() => {
    const interval = window.setInterval(() => {
      if (selectedFolderId && !document.hidden) {
        void refresh(selectedFolderId, true, true);
      }
    }, 3_000);
    return () => window.clearInterval(interval);
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
        {$t("Browse saved development sessions and the checkpoint conversation records for this workspace folder.")}
      </p>
    </div>
    <div class="flex flex-wrap items-center gap-2">
      {#if folders.length > 1}
        <label class="sr-only" for="history-folder">{$t("History folder")}</label>
        <select
          id="history-folder"
          class="tx-input max-w-56"
          value={selectedFolderId}
          onchange={(event) => void selectFolder(event.currentTarget.value)}
        >
          {#each folders as folder (folder.id)}
            <option value={folder.id}>{folder.name}</option>
          {/each}
        </select>
      {/if}
      <button
        type="button"
        class="tx-btn-ghost"
        disabled={busy || !selectedFolder}
        onclick={() => void refresh(selectedFolderId, true)}
      >
        <RefreshCw size={14} class={busy ? "inline-block animate-spin" : "inline-block"} />
        <span class="ml-1">{$t("Refresh")}</span>
      </button>
    </div>
  </div>

  {#if selectedFolder}
    <p class="tx-mono mt-3 truncate text-xs text-[var(--color-text-muted)]" title={selectedFolder.path}>
      {selectedFolder.path} · docs/history-session
    </p>
  {/if}

  {#if error}
    <p class="mt-4 rounded-lg border border-[var(--color-error)]/30 bg-[var(--color-error)]/10 px-3 py-2 text-sm text-[var(--color-error)]">
      {error}
    </p>
  {/if}

  {#if !busy && !error && sessions.length === 0}
    <div class="mt-5 rounded-lg border border-dashed border-[var(--color-border)] px-4 py-8 text-center">
      <FileText size={24} class="mx-auto text-[var(--color-text-muted)]" />
      <p class="mt-2 text-sm font-medium">{$t("No history sessions yet")}</p>
      <p class="mt-1 text-xs text-[var(--color-text-muted)]">
        {$t("A session appears here after ChatGPT completes its first history checkpoint.")}
      </p>
    </div>
  {:else}
    <div class="mt-5 grid gap-4 lg:grid-cols-[minmax(13rem,0.34fr)_minmax(0,1fr)]">
      <nav aria-label={$t("History sessions")} class="space-y-2">
        {#each sessions as session (session.number)}
          <button
            type="button"
            class={`w-full rounded-lg border px-3 py-3 text-left transition-colors ${
              selectedNumber === session.number
                ? "border-[var(--color-accent)] bg-[var(--color-accent)]/10"
                : "border-[var(--color-border)] hover:border-[var(--color-accent)]/60"
            }`}
            aria-current={selectedNumber === session.number ? "page" : undefined}
            onclick={() => void selectSession(session.number)}
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
              </div>
              {#if selectedSession?.activityDescription}
                <p class="tx-mono mt-3 truncate text-xs text-[var(--color-text-secondary)]" title={selectedSession.activityDescription}>
                  {selectedSession.activityStatus === "running"
                    ? $t("Now: {action}", { action: selectedSession.activityDescription })
                    : $t("Last: {action}", { action: selectedSession.activityDescription })}
                </p>
              {/if}
            </header>

            {#if detail.records.length > 0}
              <div class="divide-y divide-[var(--color-border)]">
                {#each detail.records as record (record.turnId)}
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
