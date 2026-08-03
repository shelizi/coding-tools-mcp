<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { open } from "@tauri-apps/plugin-dialog";
  import AppShell from "$lib/components/AppShell.svelte";
  import ToastHost from "$lib/components/ToastHost.svelte";
  import WslWorkspaceDialog from "$lib/components/WslWorkspaceDialog.svelte";
  import WorkspaceNavItem from "$lib/components/WorkspaceNavItem.svelte";
  import {
    createWorkspace,
    createWslWorkspace,
    getActionsRuntimeStatus,
    getRuntimeStatus,
    listWorkspaces,
    listWslDistributions,
  } from "$lib/api/workspaces";
  import { getLastWorkspaceId } from "$lib/api/settings";
  import { actionsRuntimeStates, mcpRuntimeStates, workspaces } from "$lib/stores/app";
  import { showToast } from "$lib/stores/toast";
  import { t } from "$lib/i18n";
  import type { RuntimeState } from "$lib/types";

  let { children } = $props();
  let wslDialogOpen = $state(false);
  let wslDistributions = $state<string[]>([]);
  let wslBusy = $state(false);
  let wslError = $state("");

  async function refreshWorkspaces() {
    const items = await listWorkspaces();
    workspaces.set(items);

    const mcpStates: Record<string, RuntimeState> = {};
    const actionsStates: Record<string, RuntimeState> = {};
    await Promise.all(
      items.map(async (item) => {
        try {
          const [mcp, actions] = await Promise.all([
            getRuntimeStatus(item.id),
            getActionsRuntimeStatus(item.id),
          ]);
          mcpStates[item.id] = mcp.state;
          actionsStates[item.id] = actions.state;
        } catch {
          mcpStates[item.id] = "stopped";
          actionsStates[item.id] = "stopped";
        }
      }),
    );
    mcpRuntimeStates.set(mcpStates);
    actionsRuntimeStates.set(actionsStates);
  }

  async function addWorkspace() {
    try {
      const selected = await open({ directory: true, multiple: false });
      if (!selected || Array.isArray(selected)) return;
      const profile = await createWorkspace(selected);
      await refreshWorkspaces();
      goto(`/workspace/${profile.id}`);
    } catch (error) {
      showToast(String(error), {
        title: $t("Failed to add workspace"),
        kind: "error",
        duration: 8000,
      });
    }
  }

  function openWorkspace(id: string) {
    goto(`/workspace/${id}`);
  }

  async function openWslWorkspaceDialog() {
    try {
      wslError = "";
      wslDistributions = await listWslDistributions();
      if (wslDistributions.length === 0) {
        throw new Error($t("No WSL distributions are installed."));
      }
      wslDialogOpen = true;
    } catch (error) {
      showToast(String(error), {
        title: $t("Failed to open WSL"),
        kind: "error",
        duration: 8000,
      });
    }
  }

  async function addWslWorkspace(distro: string, linuxPath: string, name?: string) {
    wslBusy = true;
    wslError = "";
    try {
      const profile = await createWslWorkspace(distro, linuxPath, name);
      await refreshWorkspaces();
      wslDialogOpen = false;
      goto(`/workspace/${profile.id}`);
    } catch (error) {
      wslError = String(error);
    } finally {
      wslBusy = false;
    }
  }

  function openQuickSetup() {
    goto("/quick-setup");
  }

  function openFrpSettings() {
    goto("/settings/frp");
  }

  function openSoftwareSettings() {
    goto("/settings/software");
  }

  function openGeneralSettings() {
    goto("/settings/general");
  }

  function openKeysSettings() {
    goto("/settings/keys");
  }

  onMount(async () => {
    await refreshWorkspaces();
    const path = $page.url.pathname;
    if (path === "/") {
      const lastId = await getLastWorkspaceId();
      if (lastId && $workspaces.some((item) => item.id === lastId)) {
        goto(`/workspace/${lastId}`);
      } else if ($workspaces.length > 0) {
        goto(`/workspace/${$workspaces[0].id}`);
      }
    }
  });
</script>

<AppShell
  onAddWorkspace={addWorkspace}
  onAddWslWorkspace={openWslWorkspaceDialog}
  onQuickSetup={openQuickSetup}
>
  {#snippet settingsNav()}
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/general' ? 'active' : ''}"
      onclick={openGeneralSettings}
    >
      {$t("General")}
    </button>
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/keys' ? 'active' : ''}"
      onclick={openKeysSettings}
    >
      {$t("Shared secrets")}
    </button>
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/frp' ? 'active' : ''}"
      onclick={openFrpSettings}
    >
      {$t("FRP configuration")}
    </button>
    <button
      type="button"
      class="tx-settings-link {$page.url.pathname === '/settings/software' ? 'active' : ''}"
      onclick={openSoftwareSettings}
    >
      {$t("Software management")}
    </button>
  {/snippet}
  {#snippet sidebar()}
    <div class="space-y-1">
      {#each $workspaces as workspace (workspace.id)}
        <WorkspaceNavItem
          workspace={workspace}
          active={$page.url.pathname === `/workspace/${workspace.id}`}
          mcpState={$mcpRuntimeStates[workspace.id] ?? "stopped"}
          actionsState={$actionsRuntimeStates[workspace.id] ?? "stopped"}
          onClick={() => openWorkspace(workspace.id)}
        />
      {/each}
    </div>
  {/snippet}

  {#snippet children()}
    {@render children()}
  {/snippet}
</AppShell>

<ToastHost />
<WslWorkspaceDialog
  open={wslDialogOpen}
  distributions={wslDistributions}
  busy={wslBusy}
  error={wslError}
  onClose={() => (wslDialogOpen = false)}
  onSubmit={addWslWorkspace}
/>
