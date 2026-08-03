<script lang="ts">
  import { message } from "@tauri-apps/plugin-dialog";
  import { getFrpSnippet, startTunnel, stopTunnel, type TunnelStatus } from "$lib/api/tunnel";
  import CopyButton from "$lib/components/CopyButton.svelte";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
    service: "mcp" | "actions";
    tunnelType: string;
    publicUrl: string;
    onPublicUrlChange?: (url: string) => void;
  }

  let {
    workspaceId,
    service,
    tunnelType,
    publicUrl,
    onPublicUrlChange,
  }: Props = $props();

  let status = $state<TunnelStatus | null>(null);
  let busy = $state(false);
  let frpSnippet = $state("");

  const running = $derived(status?.state === "running");
  const displayUrl = $derived(status?.publicUrl || publicUrl);

  async function toggleTunnel() {
    if (busy) return;
    busy = true;
    try {
      status = running
        ? await stopTunnel(workspaceId, service)
        : await startTunnel(workspaceId, service);
      if (status.publicUrl) {
        onPublicUrlChange?.(status.publicUrl);
      }
    } catch (error) {
      await message(String(error), { title: $t("Tunnel operation failed"), kind: "error" });
    } finally {
      busy = false;
    }
  }

  async function loadFrpSnippet() {
    try {
      frpSnippet = await getFrpSnippet(workspaceId, service);
    } catch (error) {
      await message(String(error), { title: $t("Could not generate FRP configuration"), kind: "error" });
    }
  }
</script>

<div class="tx-panel px-3 py-3">
  <div class="flex items-center justify-between gap-2">
    <div>
      <p class="text-xs font-medium text-[var(--color-text-secondary)]">{$t("Remote tunnel")}</p>
      <p class="text-[11px] text-[var(--color-text-muted)]">
        {tunnelType === "cloudflare" ? "Cloudflare" : tunnelType === "frp" ? "FRP" : $t("Not configured")}
      </p>
    </div>
    {#if tunnelType === "frp" || tunnelType === "cloudflare"}
      <button
        type="button"
        class="tx-btn-ghost px-2.5 py-1 text-xs disabled:opacity-50"
        disabled={busy}
        onclick={toggleTunnel}
      >
        {busy ? "…" : running ? $t("Disconnect") : $t("Connect")}
      </button>
    {/if}
  </div>

  {#if displayUrl}
    <div class="mt-2 flex items-center justify-between gap-2">
      <p class="truncate font-mono text-xs">{displayUrl}</p>
      <CopyButton value={displayUrl} />
    </div>
  {/if}

  {#if tunnelType === "cloudflare"}
    <p class="mt-2 text-[11px] text-[var(--color-text-muted)]">
      {$t("Cloudflare starts cloudflared automatically. Quick mode reads the trycloudflare.com URL from its logs.")}
    </p>
  {/if}

  {#if tunnelType === "frp"}
    <p class="mt-2 text-[11px] text-[var(--color-text-muted)]">
      {$t("FRP starts frpc automatically. Configure the server globally, then select it and enter a subdomain for the workspace.")}
    </p>
        <button
      type="button"
      class="mt-2 text-xs text-[var(--color-accent)] hover:underline"
      onclick={loadFrpSnippet}
    >
      {$t("Generate FRP snippet")}
    </button>
    {#if frpSnippet}
      <div class="mt-2 flex items-start justify-between gap-2">
        <pre
          class="max-h-32 min-w-0 flex-1 overflow-auto rounded border border-[var(--color-border)] p-2 font-mono text-[10px] text-[var(--color-text-secondary)]"
        >{frpSnippet}</pre>
        <CopyButton value={frpSnippet} />
      </div>
    {/if}
  {/if}
</div>
