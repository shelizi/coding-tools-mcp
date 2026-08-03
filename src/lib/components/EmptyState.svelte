<script lang="ts">
  import FolderPlus from "@lucide/svelte/icons/folder-plus";
  import { t } from "$lib/i18n";

  interface Props {
    onAdd?: () => void;
  }

  let { onAdd }: Props = $props();
</script>

<div class="flex flex-col items-center justify-center px-8 py-8 text-center">
  <div
    class="tx-card mb-6 flex h-16 w-16 items-center justify-center rounded-2xl border border-dashed"
  >
    <FolderPlus size={28} class="text-[var(--color-text-muted)]" />
  </div>
  <h2 class="text-2xl font-bold tracking-tight">{$t("Add your first workspace")}</h2>
  <p class="mt-2 max-w-md text-sm text-[var(--color-text-muted)]">
    {$t("Select a project folder, then start the MCP and Actions local services independently.")}
  </p>
  {#if onAdd}
    <button type="button" class="tx-btn-primary mt-6" onclick={onAdd}>
      {$t("Select folder")}
    </button>
  {/if}
</div>
