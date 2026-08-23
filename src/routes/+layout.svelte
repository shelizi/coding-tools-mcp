<script lang="ts">
  import "../app.css";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/stores";
  import { appUrl, routePath } from "$lib/app-path";
  import { pickDirectory, confirm } from "$lib/api/native";
  import { installHostBackend } from "$lib/backend/host";
  import { getBackend } from "$lib/backend";
  import AppShell from "$lib/components/AppShell.svelte";
  import DirectoryPicker from "$lib/components/DirectoryPicker.svelte";
  import ToastHost from "$lib/components/ToastHost.svelte";
  import WorkspaceNavItem from "$lib/components/WorkspaceNavItem.svelte";
  import {
    createWorkspace,
    getActionsRuntimeStatus,
    getRuntimeStatus,
    listWorkspaces,
  } from "$lib/api/workspaces";
  import { getLastWorkspaceId } from "$lib/api/settings";
  import { actionsRuntimeStates, mcpRuntimeStates, workspaces } from "$lib/stores/app";
  import { showToast } from "$lib/stores/toast";
  import { t } from "$lib/i18n";
  import type { RuntimeState } from "$lib/types";

  installHostBackend();

  let { children } = $props();
  const capabilities = getBackend().capabilities;
  let addWorkspacePickerOpen = $state(false);

  async function refreshWorkspaces() {
    const items = await listWorkspaces();
    workspaces.set(items);

    const mcpStates: Record<string, RuntimeState> = {};
    const actionsStates: Record<string, RuntimeState> = {};
    await Promise.all(
      items.map(async (item) => {
        try {
          const mcp = await getRuntimeStatus(item.id);
          mcpStates[item.id] = mcp.state;
        } catch {
          mcpStates[item.id] = "stopped";
        }
        if (!capabilities.actions) {
          actionsStates[item.id] = "stopped";
          return;
        }
        try {
          const actions = await getActionsRuntimeStatus(item.id);
          actionsStates[item.id] = actions.state;
        } catch {
          actionsStates[item.id] = "stopped";
        }
      }),
    );
    mcpRuntimeStates.set(mcpStates);
    actionsRuntimeStates.set(actionsStates);
  }

  async function finishAddWorkspace(selected: string) {
    const profile = await createWorkspace(selected);
    if (capabilities.agentRestart) {
      showToast($t("Workspace added. Restart the Agent to start its MCP listener."), { kind: "success" });
      await getBackend().agent.restart();
      window.setTimeout(() => window.location.reload(), 2500);
      return;
    }
    await refreshWorkspaces();
    goto(appUrl(`/workspace/${profile.id}`));
  }

  async function addWorkspace() {
    try {
      if (!capabilities.nativeDirectoryPicker) {
        addWorkspacePickerOpen = true;
        return;
      }
      const selected = await pickDirectory({ multiple: false });
      if (!selected || Array.isArray(selected)) return;
      await finishAddWorkspace(selected);
    } catch (error) {
      showToast(String(error), {
        title: $t("Failed to add workspace"),
        kind: "error",
        duration: 8000,
      });
    }
  }

  function openWorkspace(id: string) {
    goto(appUrl(`/workspace/${id}`));
  }

  function openQuickSetup() {
    const currentPath = routePath($page.url.pathname);
    const workspaceMatch = currentPath.match(/^\/workspace\/([^/]+)$/);
    const target = workspaceMatch
      ? `/quick-setup?workspace=${encodeURIComponent(workspaceMatch[1]!)}`
      : "/quick-setup";
    goto(appUrl(target));
  }

  function openFrpSettings() {
    goto(appUrl("/settings/frp"));
  }

  function openSoftwareSettings() {
    goto(appUrl("/settings/software"));
  }

  function openGeneralSettings() {
    goto(appUrl("/settings/general"));
  }

  function openKeysSettings() {
    goto(appUrl("/settings/keys"));
  }

  async function restartAgent() {
    try {
      const confirmed = await confirm(
        $t("Restart the Agent now? Active tool calls and command sessions will be stopped."),
        { kind: "warning", title: $t("Restart Agent") },
      );
      if (!confirmed) return;
      await getBackend().agent.restart();
      window.setTimeout(() => window.location.reload(), 2500);
    } catch (error) {
      showToast(String(error), { title: $t("Agent restart failed"), kind: "error", duration: 8000 });
    }
  }

  onMount(async () => {
    await refreshWorkspaces();
    const path = routePath($page.url.pathname);
    if (path === "/") {
      const lastId = await getLastWorkspaceId();
      if (lastId && $workspaces.some((item) => item.id === lastId)) {
        goto(appUrl(`/workspace/${lastId}`));
      } else if ($workspaces.length > 0) {
        goto(appUrl(`/workspace/${$workspaces[0].id}`));
      }
    }
  });
</script>

<AppShell
  onAddWorkspace={capabilities.workspaceLifecycle ? addWorkspace : undefined}
  onQuickSetup={capabilities.guidedSetup ? openQuickSetup : undefined}
>
  {#snippet settingsNav()}
    {#if capabilities.host === "desktop"}
      <button
        type="button"
        class="tx-settings-link {routePath($page.url.pathname) === '/settings/general' ? 'active' : ''}"
        onclick={openGeneralSettings}
      >
        {$t("General")}
      </button>
    {/if}
    {#if capabilities.sharedSecretStore}
      <button
        type="button"
        class="tx-settings-link {routePath($page.url.pathname) === '/settings/keys' ? 'active' : ''}"
        onclick={openKeysSettings}
      >
        {$t("Shared secrets")}
      </button>
    {/if}
    {#if capabilities.frpManagement}
      <button
        type="button"
        class="tx-settings-link {routePath($page.url.pathname) === '/settings/frp' ? 'active' : ''}"
        onclick={openFrpSettings}
      >
        {$t("FRP configuration")}
      </button>
    {/if}
    {#if capabilities.softwareManagement}
      <button
        type="button"
        class="tx-settings-link {routePath($page.url.pathname) === '/settings/software' ? 'active' : ''}"
        onclick={openSoftwareSettings}
      >
        {$t("Software management")}
      </button>
    {/if}
    {#if capabilities.agentRestart}
      <button type="button" class="tx-settings-link" onclick={() => void restartAgent()}>
        {$t("Restart Agent")}
      </button>
    {/if}
  {/snippet}
  {#snippet sidebar()}
    <div class="space-y-1">
      {#each $workspaces as workspace (workspace.id)}
        <WorkspaceNavItem
          workspace={workspace}
          active={routePath($page.url.pathname) === `/workspace/${workspace.id}`}
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

<DirectoryPicker
  open={addWorkspacePickerOpen}
  onCancel={() => (addWorkspacePickerOpen = false)}
  onSelect={async (path) => {
    addWorkspacePickerOpen = false;
    try {
      await finishAddWorkspace(path);
    } catch (error) {
      showToast(String(error), {
        title: $t("Failed to add workspace"),
        kind: "error",
        duration: 8000,
      });
    }
  }}
/>

<ToastHost />
