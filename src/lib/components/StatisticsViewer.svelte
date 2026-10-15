<script lang="ts">
  import ChartColumn from "@lucide/svelte/icons/chart-column";
  import RefreshCw from "@lucide/svelte/icons/refresh-cw";
  import { readWorkspaceTelemetry, type StatisticsPeriod, type TelemetryResult } from "$lib/api/telemetry";
  import { t } from "$lib/i18n";

  let { workspaceId }: { workspaceId: string } = $props();
  let period = $state<StatisticsPeriod>("hour");
  let refreshRevision = $state(0);
  let data = $state<TelemetryResult | null>(null);
  let busy = $state(false);
  let error = $state("");
  const statistics = $derived(data?.statistics);
  const buckets = $derived(statistics?.buckets ?? []);
  const calls = $derived(buckets.reduce((total, bucket) => total + bucket.calls, 0));
  const errors = $derived(buckets.reduce((total, bucket) => total + bucket.errors, 0));
  const peak = $derived(Math.max(1, ...buckets.map((bucket) => bucket.calls)));
  const tools = $derived(data?.aggregate?.tools ?? []);
  const records = $derived([...(data?.records ?? [])].reverse());

  function timestamp(value: number | undefined): string {
    return typeof value === "number" ? new Date(value).toLocaleString(undefined, { timeZone: "UTC" }) : $t("Unknown");
  }

  $effect(() => {
    const id = workspaceId;
    const selectedPeriod = period;
    refreshRevision;
    let active = true;
    const controller = new AbortController();
    data = null;
    error = "";
    busy = Boolean(id);
    if (id) {
      void readWorkspaceTelemetry(id, { limit: 200, statisticsPeriod: selectedPeriod }, controller.signal)
        .then((result) => {
          if (!active) return;
          if (!result.statistics) throw new Error($t("Statistics are unavailable on this backend."));
          data = result;
        })
        .catch((reason) => {
          if (active) error = String(reason);
        })
        .finally(() => {
          if (active) busy = false;
        });
    }
    return () => {
      active = false;
      controller.abort();
    };
  });
</script>

<section class="tx-card p-5" aria-busy={busy}>
  <div class="flex flex-wrap items-start justify-between gap-4">
    <div>
      <h3 class="flex items-center gap-2 font-semibold"><ChartColumn size={18} class="text-[var(--color-accent)]" />{$t("Operation statistics")}</h3>
      <p class="mt-2 max-w-2xl text-sm text-[var(--color-text-muted)]">{$t("Tool calls grouped by time. Only retained telemetry is included.")}</p>
    </div>
    <div class="flex flex-wrap items-center gap-2">
      <label class="sr-only" for="statistics-period">{$t("Time granularity")}</label>
      <select id="statistics-period" class="tx-input min-h-11 w-auto" bind:value={period}>
        <option value="minute">{$t("Per minute")}</option>
        <option value="hour">{$t("Per hour")}</option>
        <option value="day">{$t("Per day")}</option>
      </select>
      <button type="button" class="tx-btn-ghost min-h-11" disabled={busy} onclick={() => refreshRevision += 1}>
        <RefreshCw size={14} class={busy ? "inline-block motion-safe:animate-spin" : "inline-block"} />
        <span class="ml-1">{$t("Refresh")}</span>
      </button>
    </div>
  </div>

  {#if busy}
    <p class="mt-6 text-sm text-[var(--color-text-muted)]" role="status">{$t("Loading")}</p>
  {:else if error}
    <p class="mt-6 rounded-lg border border-[var(--color-error)]/30 p-4 text-sm text-[var(--color-error)]" role="alert">{error}</p>
  {:else if data && statistics}
    <div class="mt-6 grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
      <div class="rounded-lg border border-[var(--color-border)] p-4"><p class="tx-section-label">{$t("Tool calls")}</p><p class="mt-2 text-2xl font-semibold">{calls}</p></div>
      <div class="rounded-lg border border-[var(--color-border)] p-4"><p class="tx-section-label">{$t("Errors")}</p><p class="mt-2 text-2xl font-semibold text-[var(--color-error)]">{errors}</p></div>
      <div class="rounded-lg border border-[var(--color-border)] p-4"><p class="tx-section-label">{$t("Error rate")}</p><p class="mt-2 text-2xl font-semibold">{(calls ? errors / calls * 100 : 0).toFixed(1)}%</p></div>
      <div class="rounded-lg border border-[var(--color-border)] p-4"><p class="tx-section-label">{$t("Average duration")}</p><p class="mt-2 text-2xl font-semibold">{Math.round(data.aggregate?.avg_ms ?? 0)} {$t("ms")}</p></div>
    </div>

    <div class="mt-6 rounded-lg border border-[var(--color-border)] p-4">
      <div class="flex flex-wrap items-center justify-between gap-2 text-xs text-[var(--color-text-muted)]">
        <span>{timestamp(statistics.start_ts_ms)} — {timestamp(statistics.until_ts_ms)} {$t("UTC")}</span>
        <span>{$t("Peak calls: {count}", { count: Math.max(...buckets.map((bucket) => bucket.calls), 0) })}</span>
      </div>
      <div class="mt-4 overflow-x-auto" aria-hidden="true">
        <svg class="statistics-chart" viewBox="0 0 600 200" preserveAspectRatio="none">
          {#each buckets as bucket, index (bucket.started_ts_ms)}
            <g>
              <title>{$t("{time}: {calls} calls, {errors} errors", { time: timestamp(bucket.started_ts_ms), calls: bucket.calls, errors: bucket.errors })}</title>
              <rect class="statistics-calls" x={index * 600 / buckets.length} y={200 - bucket.calls / peak * 200} width={600 / buckets.length - 2} height={bucket.calls / peak * 200} rx="2" />
              <rect class="statistics-errors" x={index * 600 / buckets.length} y={200 - bucket.calls / peak * 200} width={600 / buckets.length - 2} height={bucket.errors / peak * 200} rx="2" />
            </g>
          {/each}
        </svg>
      </div>
      <div class="mt-3 flex flex-wrap items-center justify-between gap-3 text-xs text-[var(--color-text-muted)]">
        <span>{$t("UTC bucket boundaries; the current bucket may be incomplete.")}</span>
        <span class="flex items-center gap-4"><span class="flex items-center gap-1"><span class="h-2 w-2 rounded-sm bg-[var(--color-accent)]"></span>{$t("Calls")}</span><span class="flex items-center gap-1"><span class="h-2 w-2 rounded-sm bg-[var(--color-error)]"></span>{$t("Errors")}</span></span>
      </div>
      <details class="mt-4">
        <summary class="cursor-pointer py-2 text-sm">{$t("View time buckets")}</summary>
        <div class="max-h-80 overflow-auto">
          <table class="w-full text-left text-sm">
            <thead><tr><th class="py-2">{$t("Time")} {$t("UTC")}</th><th>{$t("Calls")}</th><th>{$t("Errors")}</th></tr></thead>
            <tbody>{#each buckets as bucket (bucket.started_ts_ms)}<tr class="border-t border-[var(--color-border)]"><td class="py-2">{timestamp(bucket.started_ts_ms)}</td><td>{bucket.calls}</td><td>{bucket.errors}</td></tr>{/each}</tbody>
          </table>
        </div>
      </details>
    </div>

    {#if calls === 0}
      <p class="mt-6 rounded-lg border border-dashed border-[var(--color-border)] p-8 text-center text-sm text-[var(--color-text-muted)]">{$t("No tool calls in this time range.")}</p>
    {:else}
      <h4 class="tx-section-label mt-6">{$t("Operation ranking")}</h4>
      <div class="mt-3 overflow-x-auto rounded-lg border border-[var(--color-border)]">
        <table class="w-full text-left text-sm">
          <thead class="bg-[var(--color-panel)]"><tr><th class="px-3 py-2">{$t("Tool")}</th><th class="px-3 py-2">{$t("Calls")}</th><th class="px-3 py-2">{$t("Errors")}</th></tr></thead>
          <tbody>{#each tools as tool (tool.tool)}<tr class="border-t border-[var(--color-border)]"><td class="px-3 py-2 font-mono text-xs">{tool.tool}</td><td class="px-3 py-2">{tool.calls}</td><td class="px-3 py-2">{tool.errors}</td></tr>{/each}</tbody>
        </table>
      </div>
      <div class="mt-6 flex flex-wrap items-center justify-between gap-2">
        <h4 class="tx-section-label">{$t("Recent operations")}</h4>
        <p class="text-xs text-[var(--color-text-muted)]">{$t("Showing {count} recent operations; totals include all retained calls in the range.", { count: records.length })}</p>
      </div>
      <div class="mt-3 max-h-96 overflow-auto rounded-lg border border-[var(--color-border)]">
        <table class="w-full min-w-[32rem] text-left text-sm">
          <thead class="bg-[var(--color-panel)]"><tr><th class="px-3 py-2">{$t("Time")} {$t("UTC")}</th><th class="px-3 py-2">{$t("Tool")}</th><th class="px-3 py-2">{$t("Outcome")}</th><th class="px-3 py-2">{$t("Duration")}</th></tr></thead>
          <tbody>{#each records as record, index (index)}<tr class="border-t border-[var(--color-border)]"><td class="px-3 py-2 text-xs">{timestamp(record.started_ts_ms)}</td><td class="px-3 py-2 font-mono text-xs">{record.tool ?? $t("Unknown tool")}{#if record.command_kind}<span class="ml-2 text-[var(--color-text-muted)]">{record.command_kind}</span>{/if}</td><td class="px-3 py-2 text-xs">{record.outcome_class ?? record.outcome ?? $t("Unknown")}</td><td class="px-3 py-2">{Math.round(record.duration_ms ?? 0)} {$t("ms")}</td></tr>{/each}</tbody>
        </table>
      </div>
    {/if}
    {#if data.invalid_complete_lines > 0}
      <p class="mt-4 text-xs text-[var(--color-warning)]">{$t("{count} invalid records skipped", { count: data.invalid_complete_lines })}</p>
    {/if}
    {#each data.warnings as warning}<p class="mt-2 text-xs text-[var(--color-warning)]">{warning}</p>{/each}
  {/if}
</section>

<style>
  .statistics-chart { display: block; width: 100%; height: 200px; min-width: 20rem; border-bottom: 1px solid var(--color-border); }
  .statistics-calls { fill: var(--color-accent); }
  .statistics-errors { fill: var(--color-error); }
</style>
