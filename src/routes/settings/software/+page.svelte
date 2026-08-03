<script lang="ts">
  import { onMount } from "svelte";
  import { message } from "@tauri-apps/plugin-dialog";
  import type { DownloadConfig, SoftwareStatus } from "$lib/api/software";
  import {
    listSoftware,
    installSoftware,
    uninstallSoftware,
    getDownloadConfig,
    setDownloadConfig,
  } from "$lib/api/software";
  import { t } from "$lib/i18n";

  let software = $state<SoftwareStatus[]>([]);
  let loading = $state(true);
  let installing = $state<string | null>(null);
  let uninstalling = $state<string | null>(null);

  let downloadConfig = $state<DownloadConfig>({
    githubMirror: "",
    proxyMode: "system",
    proxyUrl: "",
  });
  let configChanged = $state(false);

  async function refresh() {
    loading = true;
    try {
      software = await listSoftware();
      downloadConfig = await getDownloadConfig();
      configChanged = false;
    } finally {
      loading = false;
    }
  }

  async function install(kind: string) {
    installing = kind;
    try {
      await installSoftware(kind);
      await refresh();
    } catch (e) {
      await message(String(e), { title: $t("Installation failed"), kind: "error" });
    } finally {
      installing = null;
    }
  }

  async function uninstall(kind: string) {
    uninstalling = kind;
    try {
      await uninstallSoftware(kind);
      await refresh();
    } catch (e) {
      await message(String(e), { title: $t("Uninstallation failed"), kind: "error" });
    } finally {
      uninstalling = null;
    }
  }

  async function saveConfig() {
    try {
      await setDownloadConfig(downloadConfig);
      configChanged = false;
      await message($t("Download settings saved."), { title: $t("Saved"), kind: "info" });
    } catch (e) {
      await message(String(e), { title: $t("Failed to save"), kind: "error" });
    }
  }

  onMount(refresh);
</script>

<section class="page-scroll">
  <header class="page-header">
    <p class="page-kicker">{$t("Global settings")}</p>
    <h2 class="page-title">{$t("Software management")}</h2>
    <p class="mt-2 max-w-2xl text-sm text-[var(--color-text-muted)]">
      {$t(
        "Install and remove the frpc and cloudflared tunnel clients. Managed binaries are stored in the application cache.",
      )}
    </p>
  </header>

  <div class="page-body flex flex-col gap-6">
    <!-- Binary status -->
    <div class="tx-card p-4">
      <h3 class="text-sm font-semibold">{$t("Status")}</h3>
      {#if loading}
        <p class="mt-4 text-sm text-[var(--color-text-muted)]">{$t("Loading…")}</p>
      {:else if software.length === 0}
        <p class="mt-4 text-sm text-[var(--color-text-muted)]">{$t("No information available.")}</p>
      {:else}
        <ul class="mt-4 space-y-2">
          {#each software as s (s.kind)}
            <li class="tx-panel flex items-center justify-between gap-3 px-3 py-2">
              <div class="min-w-0">
                <p class="text-sm font-medium">{s.name}</p>
                <p class="font-mono text-xs text-[var(--color-text-muted)]">
                  {s.installed ? s.path : $t("Not installed")}
                  · {s.managed ? $t("Managed") : $t("System installation")}
                </p>
              </div>
              <div class="flex shrink-0 gap-2">
                {#if s.installed}
                  {#if s.managed}
                    <button
                      type="button"
                      class="text-xs text-red-400 hover:underline disabled:opacity-50"
                      disabled={uninstalling === s.kind}
                      onclick={() => uninstall(s.kind)}
                    >
                      {uninstalling === s.kind ? $t("Uninstalling…") : $t("Uninstall")}
                    </button>
                  {:else}
                    <span class="text-xs text-[var(--color-text-muted)]">{$t("System installation")}</span>
                  {/if}
                {:else}
                  <button
                    type="button"
                    class="text-xs text-[var(--color-accent)] hover:underline disabled:opacity-50"
                    disabled={installing === s.kind}
                    onclick={() => install(s.kind)}
                  >
                    {installing === s.kind ? $t("Installing…") : $t("Install")}
                  </button>
                {/if}
              </div>
            </li>
          {/each}
        </ul>
      {/if}
    </div>

    <!-- Download config -->
    <div class="tx-card p-4">
      <h3 class="text-sm font-semibold">{$t("Download settings")}</h3>
      <form
        class="mt-4 grid gap-3"
        onsubmit={(e) => { e.preventDefault(); void saveConfig(); }}
      >
        <label class="grid gap-1">
          <span class="text-xs text-[var(--color-text-muted)]">{$t("GitHub mirror")}</span>
          <input
            type="text"
            class="tx-input tx-mono"
            placeholder="https://your-trusted-mirror.example"
            bind:value={downloadConfig.githubMirror}
            oninput={() => (configChanged = true)}
          />
          <span class="text-xs text-[var(--color-text-muted)]">{$t("Leave empty to use the official GitHub release directly. A configured mirror is fallback only and downloaded binaries are still verified.")}</span>
        </label>
        <label class="grid gap-1">
          <span class="text-xs text-[var(--color-text-muted)]">{$t("Proxy mode")}</span>
          <select
            class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
            bind:value={downloadConfig.proxyMode}
            onchange={() => (configChanged = true)}
          >
            <option value="system">{$t("System proxy (default)")}</option>
            <option value="none">{$t("No proxy")}</option>
            <option value="manual">{$t("Manual proxy URL")}</option>
          </select>
        </label>
        {#if downloadConfig.proxyMode === "manual"}
          <label class="grid gap-1">
            <span class="text-xs text-[var(--color-text-muted)]">{$t("Proxy URL")}</span>
            <input
              type="text"
              class="tx-input tx-mono"
              placeholder="http://127.0.0.1:7890"
              bind:value={downloadConfig.proxyUrl}
              oninput={() => (configChanged = true)}
            />
          </label>
        {/if}
        <div class="flex justify-end pt-1">
          <button
            type="submit"
            class="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
            disabled={!configChanged}
          >
            {$t("Save settings")}
          </button>
        </div>
      </form>
    </div>
  </div>
</section>
