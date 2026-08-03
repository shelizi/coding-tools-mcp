<script lang="ts">
  import LanguageSelect from "$lib/components/LanguageSelect.svelte";
  import ThemeToggle from "$lib/components/ThemeToggle.svelte";
  import { APP_VERSION } from "$lib/app-version";
  import { t } from "$lib/i18n";
  import type { Snippet } from "svelte";

  interface Props {
    children: Snippet;
    sidebar: Snippet;
    onAddWorkspace?: () => void | Promise<void>;
    onAddWslWorkspace?: () => void | Promise<void>;
    onQuickSetup?: () => void | Promise<void>;
    settingsNav?: Snippet;
  }

  let { children, sidebar, onAddWorkspace, onAddWslWorkspace, onQuickSetup, settingsNav }: Props = $props();
</script>

<div class="app-layout">
  <aside class="tx-sidebar">
    <div class="tx-sidebar-header">
      <div>
        <p class="tx-brand-kicker">Coding Tools</p>
        <h1 class="tx-brand-title">{$t("Desktop Console")}</h1>
      </div>
      <div class="mt-3 flex items-center justify-between gap-2">
        <LanguageSelect />
        <ThemeToggle />
      </div>
      {#if onQuickSetup}
        <button type="button" class="tx-btn-primary tx-btn-sidebar" onclick={onQuickSetup}>
          {$t("Quick setup")}
        </button>
      {/if}
      {#if onAddWorkspace}
        <button type="button" class="tx-btn-primary tx-btn-sidebar" onclick={onAddWorkspace}>
          {$t("Add workspace")}
        </button>
      {/if}
      {#if onAddWslWorkspace}
        <button type="button" class="tx-btn-ghost tx-btn-sidebar tx-btn-sidebar-secondary" onclick={onAddWslWorkspace}>
          {$t("Add WSL workspace")}
        </button>
      {/if}
    </div>

    <div class="tx-sidebar-body">
      {#if onAddWorkspace}
        <p class="tx-sidebar-section-label">{$t("Workspaces")}</p>
      {/if}
      {@render sidebar()}
    </div>

    {#if settingsNav}
      <div class="tx-sidebar-footer">
        <p class="tx-sidebar-section-label">{$t("Settings")}</p>
        {@render settingsNav()}
        <p class="tx-app-version">v{APP_VERSION}</p>
      </div>
    {:else}
      <div class="tx-sidebar-footer">
        <p class="tx-app-version">v{APP_VERSION}</p>
      </div>
    {/if}
  </aside>

  <main class="tx-main">
    {@render children()}
  </main>
</div>

<svelte:head>
  <title>Coding Tools MCP</title>
</svelte:head>
