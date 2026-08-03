<script lang="ts">
  import StatusOrb from "$lib/components/StatusOrb.svelte";
  import { t } from "$lib/i18n";
  import {
    actionsLocalEndpoint,
    actionsOpenApiUrl,
    mcpLocalEndpoint,
    type ActionsConfig,
    type RuntimeState,
    type WorkspaceProfile,
  } from "$lib/types";
  import type { FrpProfileDto } from "$lib/api/settings";

  interface Props {
    profile: WorkspaceProfile;
    actions: ActionsConfig;
    mcpStatus: RuntimeState;
    actionsStatus: RuntimeState;
    mcpBusy: boolean;
    actionsBusy: boolean;
    mcpLocal: string;
    mcpPublic: string;
    actionsLocal: string;
    actionsPublic: string;
    frpProfiles: FrpProfileDto[];
    stateLabel: (state: RuntimeState) => string;
    onToggleMcp: () => void | Promise<void>;
    onToggleActions: () => void | Promise<void>;
    onNavigate: (tab: "mcp" | "actions" | "settings") => void;
  }

  let {
    profile,
    actions,
    mcpStatus,
    actionsStatus,
    mcpBusy,
    actionsBusy,
    mcpLocal,
    mcpPublic,
    actionsLocal,
    actionsPublic,
    frpProfiles,
    stateLabel,
    onToggleMcp,
    onToggleActions,
    onNavigate,
  }: Props = $props();
</script>

<div class="grid gap-4 lg:grid-cols-2">
  <article class="tx-card flex flex-col gap-4 p-5">
    <div class="flex items-start justify-between gap-4">
      <div>
        <div class="flex items-center gap-2">
          <StatusOrb state={mcpStatus} />
          <h3 class="text-base font-semibold">MCP</h3>
        </div>
        <p class="mt-1 text-sm text-[var(--color-text-muted)]">
          Streamable HTTP · {$t("Tool runtime")}
        </p>
      </div>
      <span class="tx-status-pill cursor-default">{stateLabel(mcpStatus)}</span>
    </div>

    <div class="grid gap-2 text-sm">
      <div>
        <p class="tx-section-label">{$t("Local endpoint")}</p>
        <p class="break-all text-[var(--color-text-secondary)]">
          {mcpLocal || mcpLocalEndpoint(profile.runtime.local_port, profile.runtime.bind_address)}
        </p>
      </div>
      {#if mcpPublic}
        <div>
          <p class="tx-section-label">{$t("Public endpoint")}</p>
          <p class="break-all text-[var(--color-text-secondary)]">{mcpPublic}</p>
        </div>
      {/if}
    </div>

    <div class="mt-auto flex flex-wrap gap-2">
      <button
        type="button"
        class="tx-btn-primary"
        class:tx-btn-danger={mcpStatus === "running"}
        disabled={mcpBusy || mcpStatus === "starting" || mcpStatus === "stopping"}
        onclick={() => void onToggleMcp()}
      >
        {#if mcpBusy}
          {$t("Working…")}
        {:else if mcpStatus === "running"}
          {$t("Stop")}
        {:else}
          {$t("Start")}
        {/if}
      </button>
      <button type="button" class="tx-btn-secondary" onclick={() => onNavigate("mcp")}>
        {$t("Manage MCP")}
      </button>
    </div>
  </article>

  <article class="tx-card flex flex-col gap-4 p-5">
    <div class="flex items-start justify-between gap-4">
      <div>
        <div class="flex items-center gap-2">
          <StatusOrb state={actionsStatus} />
          <h3 class="text-base font-semibold">Actions</h3>
        </div>
        <p class="mt-1 text-sm text-[var(--color-text-muted)]">
          {$t("OpenAPI gateway · ChatGPT Actions")}
        </p>
      </div>
      <span class="tx-status-pill cursor-default">{stateLabel(actionsStatus)}</span>
    </div>

    <div class="grid gap-2 text-sm">
      <div>
        <p class="tx-section-label">{$t("Local endpoint")}</p>
        <p class="break-all text-[var(--color-text-secondary)]">
          {actionsLocal || actionsLocalEndpoint(actions.local_port, actions.bind_address)}
        </p>
      </div>
      {#if actionsPublic || actionsOpenApiUrl(profile, frpProfiles)}
        <div>
          <p class="tx-section-label">OpenAPI</p>
          <p class="break-all text-[var(--color-text-secondary)]">
            {actionsPublic || actionsOpenApiUrl(profile, frpProfiles)}
          </p>
        </div>
      {/if}
    </div>

    <div class="mt-auto flex flex-wrap gap-2">
      <button
        type="button"
        class="tx-btn-primary"
        class:tx-btn-danger={actionsStatus === "running"}
        disabled={actionsBusy || actionsStatus === "starting" || actionsStatus === "stopping"}
        onclick={() => void onToggleActions()}
      >
        {#if actionsBusy}
          {$t("Working…")}
        {:else if actionsStatus === "running"}
          {$t("Stop")}
        {:else}
          {$t("Start")}
        {/if}
      </button>
      <button type="button" class="tx-btn-secondary" onclick={() => onNavigate("actions")}>
        {$t("Manage Actions")}
      </button>
    </div>
  </article>
</div>

<div class="tx-card mt-4 flex flex-wrap items-center justify-between gap-4 p-5">
  <div>
    <p class="tx-section-label">{$t("Current working directory")}</p>
    <p class="mt-1 break-all font-medium">{profile.path}</p>
    <p class="mt-1 text-sm text-[var(--color-text-muted)]">
      {$t("Workspace name, project folders, and ChatGPT session recovery are managed on the settings page.")}
    </p>
  </div>
  <button type="button" class="tx-btn-secondary" onclick={() => onNavigate("settings")}>
    {$t("Open workspace settings")}
  </button>
</div>
