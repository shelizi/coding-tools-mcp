<script lang="ts">
  import Activity from "@lucide/svelte/icons/activity";
  import AlertTriangle from "@lucide/svelte/icons/alert-triangle";
  import Clock3 from "@lucide/svelte/icons/clock-3";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import {
    readWorkspaceTelemetry,
    type TelemetryRecord,
    type TelemetryResult,
  } from "$lib/api/telemetry";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
  }

  let { workspaceId }: Props = $props();
  let data = $state<TelemetryResult | null>(null);
  let busy = $state(false);
  let error = $state("");
  let errorsOnly = $state(false);
  let limit = $state(100);
  let loadedWorkspaceId = "";

  const aggregate = $derived(data?.aggregate);
  const records = $derived(data?.records ?? []);
  const tools = $derived(aggregate?.tools ?? []);

  function formatTimestamp(value: unknown): string {
    if (typeof value !== "number" || value <= 0) return $t("Unknown");
    return new Date(value).toLocaleString();
  }

  function formatMs(value: unknown): string {
    return typeof value === "number" ? `${Math.round(value)} ms` : "—";
  }

  function recordTitle(record: TelemetryRecord): string {
    if (record.event === "async_session_finalized") {
      return record.command_kind ? `async · ${record.command_kind}` : "async session";
    }
    return record.tool ?? $t("Unknown tool");
  }

  function recordOutcome(record: TelemetryRecord): string {
    return record.outcome_class ?? record.outcome ?? (record.event === "async_session_finalized" ? "completed" : "unknown");
  }

  function recordDetails(record: TelemetryRecord): string {
    return JSON.stringify(record, null, 2);
  }

  async function refresh() {
    if (!workspaceId || busy) return;
    busy = true;
    error = "";
    try {
      data = await readWorkspaceTelemetry(workspaceId, {
        limit,
        errorsOnly,
      });
    } catch (reason) {
      error = String(reason);
      data = null;
    } finally {
      busy = false;
    }
  }

  $effect(() => {
    if (!workspaceId || workspaceId === loadedWorkspaceId) return;
    loadedWorkspaceId = workspaceId;
    void refresh();
  });
</script>

<section class="tx-card p-5">
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div>
      <div class="flex items-center gap-2">
        <Activity size={18} class="text-[var(--color-accent)]" />
        <h3 class="font-semibold">{$t("Operation telemetry")}</h3>
      </div>
      <p class="mt-1 max-w-2xl text-sm text-[var(--color-text-muted)]">
        {$t("Browse sanitized MCP tool calls, timings, outcomes, and errors for this workspace.")}
      </p>
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <label class="inline-flex items-center gap-2 text-xs text-[var(--color-text-secondary)]">
        <input type="checkbox" bind:checked={errorsOnly} onchange={() => void refresh()} />
        {$t("Errors only")}
      </label>
      <label class="sr-only" for="telemetry-limit">{$t("Telemetry records")}</label>
      <select
        id="telemetry-limit"
        class="tx-input w-auto"
        value={String(limit)}
        onchange={(event) => {
          limit = Number(event.currentTarget.value);
          void refresh();
        }}
      >
        <option value="50">50</option>
        <option value="100">100</option>
        <option value="200">200</option>
      </select>
      <button type="button" class="tx-btn-ghost" disabled={busy} onclick={() => void refresh()}>
        <RefreshCw size={14} class={busy ? "inline-block animate-spin" : "inline-block"} />
        <span class="ml-1">{$t("Refresh")}</span>
      </button>
    </div>
  </div>

  {#if data?.log_dir}
    <p class="tx-mono mt-3 truncate text-xs text-[var(--color-text-muted)]" title={data.log_dir}>
      {data.log_dir}
    </p>
  {/if}

  {#if error}
    <p class="mt-4 rounded-lg border border-[var(--color-error)]/30 bg-[var(--color-error)]/10 px-3 py-2 text-sm text-[var(--color-error)]">
      {error}
    </p>
  {/if}

  {#if data && aggregate}
    <div class="mt-5 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      <div class="rounded-lg border border-[var(--color-border)] p-3"><p class="tx-section-label">{$t("Tool calls")}</p><p class="mt-1 text-xl font-semibold">{aggregate.calls}</p></div>
      <div class="rounded-lg border border-[var(--color-border)] p-3"><p class="tx-section-label">{$t("Errors")}</p><p class="mt-1 text-xl font-semibold text-[var(--color-error)]">{aggregate.errors}</p></div>
      <div class="rounded-lg border border-[var(--color-border)] p-3"><p class="tx-section-label">{$t("Average duration")}</p><p class="mt-1 text-xl font-semibold">{formatMs(aggregate.avg_ms)}</p></div>
      <div class="rounded-lg border border-[var(--color-border)] p-3"><p class="tx-section-label">{$t("P95 duration")}</p><p class="mt-1 text-xl font-semibold">{formatMs(aggregate.p95_ms)}</p></div>
    </div>

    {#if tools.length > 0}
      <div class="mt-5 overflow-x-auto rounded-lg border border-[var(--color-border)]">
        <table class="w-full min-w-[38rem] text-left text-sm">
          <thead class="border-b border-[var(--color-border)] bg-[var(--color-panel)] text-xs text-[var(--color-text-muted)]">
            <tr><th class="px-3 py-2 font-medium">{$t("Tool")}</th><th class="px-3 py-2 font-medium">{$t("Calls")}</th><th class="px-3 py-2 font-medium">{$t("Errors")}</th><th class="px-3 py-2 font-medium">P95</th><th class="px-3 py-2 font-medium">{$t("Max")}</th></tr>
          </thead>
          <tbody class="divide-y divide-[var(--color-border)]">
            {#each tools as item (item.tool)}
              <tr><td class="px-3 py-2 font-mono text-xs">{item.tool}</td><td class="px-3 py-2">{item.calls}</td><td class="px-3 py-2">{item.errors}</td><td class="px-3 py-2">{formatMs(item.p95_ms)}</td><td class="px-3 py-2">{formatMs(item.max_ms)}</td></tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}

    <div class="mt-5 flex items-center justify-between gap-3">
      <div><p class="tx-section-label">{$t("Recent operations")}</p><p class="mt-1 text-xs text-[var(--color-text-muted)]">{$t("{count} records", { count: records.length })}</p></div>
      {#if data.invalid_complete_lines > 0}
        <span class="inline-flex items-center gap-1 text-xs text-[var(--color-warning)]"><AlertTriangle size={13} /> {$t("{count} invalid records skipped", { count: data.invalid_complete_lines })}</span>
      {/if}
    </div>

    {#if records.length > 0}
      <div class="mt-3 space-y-2">
        {#each records as record, index (`${record.started_ts_ms ?? 0}-${record.tool ?? record.event ?? "record"}-${index}`)}
          <details class="rounded-lg border border-[var(--color-border)] bg-[var(--color-bg)]">
            <summary class="cursor-pointer px-3 py-3 text-sm">
              <span class="inline-flex min-w-0 items-center gap-2"><Clock3 size={13} class="shrink-0 text-[var(--color-text-muted)]" /><span class="truncate font-mono text-xs">{recordTitle(record)}</span><span class="text-xs text-[var(--color-text-muted)]">{formatTimestamp(record.started_ts_ms)}</span></span>
              <span class="float-right ml-2 text-xs text-[var(--color-text-muted)]">{formatMs(record.duration_ms)} · {recordOutcome(record)}</span>
            </summary>
            {#if record.command_preview}
              <p class="border-t border-[var(--color-border)] px-3 py-2 font-mono text-xs text-[var(--color-text-secondary)]">{record.command_preview}</p>
            {/if}
            <pre class="max-h-80 overflow-auto whitespace-pre-wrap break-words border-t border-[var(--color-border)] p-3 font-mono text-xs leading-relaxed">{recordDetails(record)}</pre>
          </details>
        {/each}
      </div>
    {:else}
      <p class="mt-4 rounded-lg border border-dashed border-[var(--color-border)] px-4 py-8 text-center text-sm text-[var(--color-text-muted)]">{$t("No telemetry records yet")}</p>
    {/if}
  {:else if !busy && !error}
    <p class="mt-5 rounded-lg border border-dashed border-[var(--color-border)] px-4 py-8 text-center text-sm text-[var(--color-text-muted)]">{$t("No telemetry records yet")}</p>
  {/if}
</section>
