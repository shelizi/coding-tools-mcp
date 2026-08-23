<script lang="ts">
  import { getBackend } from "$lib/backend";
  import type { OperationLogPayload, OperationLogQuery } from "$lib/backend";
  import { t } from "$lib/i18n";
  import type { WorkspaceFolder } from "$lib/types";

  interface Props {
    workspaceId: string;
    folders: WorkspaceFolder[];
  }

  let { workspaceId, folders }: Props = $props();

  let folderId = $state("");
  let status = $state<OperationLogQuery["status"]>("all");
  let tool = $state("");
  let errorsOnly = $state(false);
  let limit = $state(50);
  const filters = $derived<OperationLogQuery>({
    folderId,
    status,
    tool,
    errorsOnly,
    limit,
  });
  let pages = $state<OperationLogPayload[]>([]);
  let busy = $state(false);
  let loadingOlder = $state(false);
  let error = $state("");
  let generation = 0;

  const activeFolderId = $derived(
    folders.some((folder) => folder.id === filters.folderId) ? filters.folderId : (folders[0]?.id ?? ""),
  );
  const data = $derived.by(() => {
    if (!pages.length) return null;
    const first = pages[0]!;
    const last = pages[pages.length - 1]!;
    return {
      ...first,
      nextCursor: last.nextCursor,
      operations: pages.flatMap((page) => page.operations),
    };
  });

  async function load(reset: boolean) {
    if (!workspaceId || !activeFolderId) return;
    const current = ++generation;
    if (reset) {
      busy = true;
      pages = [];
    } else {
      loadingOlder = true;
    }
    error = "";
    try {
      const cursor = reset ? 0 : (pages.at(-1)?.nextCursor ?? 0);
      const payload = await getBackend().operations.query(
        workspaceId,
        { ...filters, folderId: activeFolderId },
        cursor,
      );
      if (current !== generation) return;
      pages = reset ? [payload] : [...pages, payload];
    } catch (reason) {
      if (current !== generation) return;
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      if (current === generation) {
        busy = false;
        loadingOlder = false;
      }
    }
  }

  $effect(() => {
    void workspaceId;
    void activeFolderId;
    void filters.status;
    void filters.tool;
    void filters.errorsOnly;
    void filters.limit;
    void load(true);
  });

  function setTool(value: string) {
    if (/^[A-Za-z0-9._-]*$/.test(value)) tool = value;
  }
</script>

<div class="grid gap-4">
  <section class="tx-card p-5">
    <div class="flex flex-wrap items-start justify-between gap-3">
      <div>
        <p class="tx-section-label">{$t("Logs")}</p>
        <h3 class="text-base font-semibold">{$t("Operation log")}</h3>
        <p class="mt-1 max-w-3xl text-sm text-[var(--color-text-muted)]">
          {$t("Browse persisted operation starts, completions, failures, and interrupted records without exposing commands or output.")}
        </p>
      </div>
      <button type="button" class="tx-btn-ghost" disabled={busy} onclick={() => void load(true)}>
        {busy ? $t("Refreshing…") : $t("Refresh")}
      </button>
    </div>

    <div class="mt-4 grid gap-3 md:grid-cols-2 lg:grid-cols-5">
      <label class="text-xs text-[var(--color-text-muted)]">
        {$t("Log folder")}
        <select
          class="tx-input mt-1 w-full"
          value={activeFolderId}
          onchange={(event) => (folderId = event.currentTarget.value)}
        >
          {#each folders as folder (folder.id)}
            <option value={folder.id}>{folder.name}</option>
          {/each}
        </select>
      </label>
      <label class="text-xs text-[var(--color-text-muted)]">
        {$t("Status")}
        <select
          class="tx-input mt-1 w-full"
          value={filters.status}
          onchange={(event) =>
            (status = event.currentTarget.value as OperationLogQuery["status"])}
        >
          <option value="all">{$t("All statuses")}</option>
          <option value="completed">{$t("Completed")}</option>
          <option value="failed">{$t("Failed")}</option>
          <option value="incomplete">{$t("Incomplete")}</option>
        </select>
      </label>
      <label class="text-xs text-[var(--color-text-muted)]">
        {$t("Tool filter")}
        <input
          class="tx-input mt-1 w-full"
          value={filters.tool}
          oninput={(event) => setTool(event.currentTarget.value)}
        />
      </label>
      <label class="text-xs text-[var(--color-text-muted)]">
        {$t("Records")}
        <select
          class="tx-input mt-1 w-full"
          value={String(filters.limit)}
          onchange={(event) => (limit = Number(event.currentTarget.value))}
        >
          <option value="25">25</option>
          <option value="50">50</option>
          <option value="100">100</option>
          <option value="200">200</option>
        </select>
      </label>
      <label class="mt-6 flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          checked={filters.errorsOnly}
          onchange={(event) => (errorsOnly = event.currentTarget.checked)}
        />
        {$t("Failures and incomplete only")}
      </label>
    </div>
  </section>

  {#if error}
    <p class="text-sm text-[var(--danger)]">{error}</p>
  {/if}

  <div class="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
    <article class="tx-card p-4">
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Operations")}</p>
      <p class="mt-1 text-xl font-semibold">{data?.summary.total ?? 0}</p>
      <p class="text-xs text-[var(--color-text-muted)]">{data?.matched ?? 0} {$t("matched")}</p>
    </article>
    <article class="tx-card p-4">
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Completed")}</p>
      <p class="mt-1 text-xl font-semibold">{data?.summary.completed ?? 0}</p>
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Terminal success records")}</p>
    </article>
    <article class="tx-card p-4">
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Failed")}</p>
      <p class="mt-1 text-xl font-semibold">{data?.summary.failed ?? 0}</p>
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Terminal failure records")}</p>
    </article>
    <article class="tx-card p-4">
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Incomplete")}</p>
      <p class="mt-1 text-xl font-semibold">{data?.summary.incomplete ?? 0}</p>
      <p class="text-xs text-[var(--color-text-muted)]">{$t("Started without a terminal record")}</p>
    </article>
  </div>

  <section class="tx-card p-5">
    <h3 class="text-base font-semibold">{$t("Recent operations")}</h3>
    <div class="mt-3 grid gap-2">
      {#each data?.operations ?? [] as operation (operation.id)}
        <details class="rounded-[12px] border border-[var(--color-border)] px-3 py-2">
          <summary class="cursor-pointer text-sm">
            <span class="font-medium">{operation.tool}</span>
            <span class="ml-2 text-[var(--color-text-muted)]">{operation.status}</span>
            {#if operation.durationMs != null}
              <span class="ml-2 text-[var(--color-text-muted)]">{operation.durationMs}</span>
            {/if}
          </summary>
          {#if operation.reason}
            <p class="mt-2 text-sm text-[var(--color-text-secondary)]">{operation.reason}</p>
          {/if}
        </details>
      {:else}
        <p class="text-sm text-[var(--color-text-muted)]">{$t("No operations match the current filters.")}</p>
      {/each}
    </div>
    {#if data?.nextCursor != null}
      <button
        type="button"
        class="tx-btn-ghost mt-4"
        disabled={loadingOlder}
        onclick={() => void load(false)}
      >
        {$t("Load older")}
      </button>
    {/if}
  </section>
</div>
