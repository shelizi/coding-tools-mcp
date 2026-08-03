<script lang="ts">
  import { t } from "$lib/i18n";
  import ServiceStatusPair from "$lib/components/ServiceStatusPair.svelte";
  import { workspaceFolders, type RuntimeState, type WorkspaceProfile } from "$lib/types";

  interface Props {
    workspace: WorkspaceProfile;
    active: boolean;
    mcpState: RuntimeState;
    actionsState: RuntimeState;
    onClick: () => void;
  }

  let { workspace, active, mcpState, actionsState, onClick }: Props = $props();
  const folderCount = $derived(workspaceFolders(workspace).length);
  const wslDistro = $derived(
    workspaceFolders(workspace).find((folder) => folder.execution?.kind === "wsl")?.execution,
  );
</script>

<div class="tx-nav-item" class:active>
  <button type="button" class="tx-nav-button" onclick={onClick}>
    <ServiceStatusPair mcp={mcpState} actions={actionsState} />
    <span class="min-w-0 flex-1">
      <span class="block truncate text-sm font-medium">{workspace.name}</span>
      <span class="block truncate text-[11px] text-[var(--color-text-muted)]">
        {#if wslDistro?.kind === "wsl"}
          {$t("WSL")} · {wslDistro.distro} · {$t("{count} folders", { count: folderCount })}
        {:else}
          {$t("{count} folders", { count: folderCount })}
        {/if}
      </span>
    </span>
  </button>
</div>
