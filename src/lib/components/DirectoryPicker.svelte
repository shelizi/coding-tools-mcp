<script lang="ts">
  import { getBackend } from "$lib/backend";
  import { t } from "$lib/i18n";
  import type { DirectoryBrowseResult } from "$lib/backend";

  interface Props {
    open: boolean;
    workspaceId?: string;
    initialPath?: string;
    onCancel: () => void;
    onSelect: (path: string) => void | Promise<void>;
  }

  let { open, workspaceId, initialPath = "", onCancel, onSelect }: Props = $props();
  let payload = $state<DirectoryBrowseResult | null>(null);
  let location = $state("");
  let loading = $state(false);
  let error = $state("");
  let generation = 0;

  async function loadDirectory(target?: string) {
    const current = ++generation;
    loading = true;
    error = "";
    try {
      const result = await getBackend().directories.browse(target?.trim() || undefined, workspaceId);
      if (current !== generation) return;
      payload = result;
      location = result.path;
    } catch (reason) {
      if (current !== generation) return;
      error = reason instanceof Error ? reason.message : String(reason);
    } finally {
      if (current === generation) loading = false;
    }
  }

  $effect(() => {
    if (!open) return;
    payload = null;
    location = initialPath;
    void loadDirectory(initialPath);
    return () => {
      generation += 1;
    };
  });
</script>

{#if open}
  <div class="directory-picker-backdrop" role="presentation">
    <div class="tx-card directory-picker" role="dialog" aria-modal="true" aria-labelledby="directory-picker-title">
      <header class="flex items-start justify-between gap-3">
        <h3 id="directory-picker-title" class="text-base font-semibold">{$t("Select folder")}</h3>
        <button type="button" class="tx-btn-ghost" onclick={onCancel}>{$t("Cancel")}</button>
      </header>

      <form
        class="mt-4 flex gap-2"
        onsubmit={(event) => {
          event.preventDefault();
          void loadDirectory(location);
        }}
      >
        <label class="sr-only" for="directory-picker-location">{$t("Absolute path")}</label>
        <input
          id="directory-picker-location"
          class="tx-input min-w-0 flex-1"
          value={location}
          oninput={(event) => (location = event.currentTarget.value)}
          autocomplete="off"
        />
        <button type="submit" class="tx-btn-ghost" disabled={loading || !location.trim()}>{$t("Open")}</button>
      </form>

      {#if payload?.roots.length}
        <div class="mt-3 flex flex-wrap items-center gap-2">
          <span class="text-xs text-[var(--color-text-muted)]">{$t("Roots")}</span>
          {#each payload.roots as root (root)}
            <button
              type="button"
              class="tx-btn-ghost px-2 py-1 text-xs"
              disabled={loading || root === payload.path}
              onclick={() => void loadDirectory(root)}
            >
              {root}
            </button>
          {/each}
        </div>
      {/if}

      {#if error}
        <p class="mt-3 text-sm text-[var(--danger)]">{error}</p>
      {/if}
      {#if payload?.truncated}
        <p class="mt-3 text-sm text-[var(--color-text-muted)]">
          {$t("This directory has {count} subfolders; showing the first 2,000.", { count: payload.totalDirectories })}
        </p>
      {/if}

      <div class="mt-4 max-h-72 overflow-auto rounded-[12px] border border-[var(--color-border)]">
        {#if payload?.parent}
          <button
            type="button"
            class="block w-full truncate px-3 py-2 text-left text-sm hover:bg-[var(--surface-hover)]"
            disabled={loading}
            onclick={() => void loadDirectory(payload?.parent ?? undefined)}
          >
            {$t("Parent directory")}
          </button>
        {/if}
        {#each payload?.directories ?? [] as directory (directory.path)}
          <button
            type="button"
            class="block w-full truncate px-3 py-2 text-left text-sm hover:bg-[var(--surface-hover)]"
            disabled={loading}
            onclick={() => void loadDirectory(directory.path)}
          >
            {directory.name}
          </button>
        {/each}
      </div>

      <footer class="mt-4 flex justify-end gap-2">
        <button type="button" class="tx-btn-ghost" onclick={onCancel}>{$t("Cancel")}</button>
        <button
          type="button"
          class="tx-btn-primary"
          disabled={!payload?.path || loading}
          onclick={() => payload && onSelect(payload.path)}
        >
          {$t("Choose this folder")}
        </button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .directory-picker-backdrop {
    position: fixed;
    inset: 0;
    z-index: 40;
    display: grid;
    place-items: center;
    background: rgba(15, 23, 42, 0.45);
    padding: 1.5rem;
  }
  .directory-picker {
    width: min(40rem, 100%);
    padding: 1.25rem;
  }
</style>
