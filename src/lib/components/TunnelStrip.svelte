<script lang="ts">
  import { onMount } from "svelte";
  import { alert as message } from "$lib/api/native";
  import { getFrpSnippet, getTunnelStatus, startTunnel, stopTunnel, type TunnelStatus } from "$lib/api/tunnel";
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
  const builtin = $derived(tunnelType === "builtin");
  const transportLabel = $derived(status?.transportMode || (builtin && running ? "Built-in WSS" : "—"));

  async function refreshStatus() {
    if (tunnelType === "none") return;
    try {
      status = await getTunnelStatus(workspaceId, service);
    } catch {
      // Keep the last known status while the runtime or management endpoint restarts.
    }
  }

  onMount(() => {
    void refreshStatus();
    const timer = setInterval(() => void refreshStatus(), 2000);
    return () => clearInterval(timer);
  });

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
        {builtin ? $t("Built-in WSS tunnel") : tunnelType === "cloudflare" ? "Cloudflare" : tunnelType === "frp" ? "FRP" : $t("Not configured")}
      </p>
    </div>
    {#if builtin || tunnelType === "frp" || tunnelType === "cloudflare"}
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

  {#if builtin}
    <div class="mt-3 grid grid-cols-2 gap-x-4 gap-y-2 text-[11px]">
      <div><span class="text-[var(--color-text-muted)]">State</span><p class="font-medium">{status?.state ?? "—"}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Transport</span><p class="font-medium">{transportLabel}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Control</span><p>{status?.controlConnected == null ? "—" : status.controlConnected ? "connected" : "disconnected"}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Data connections</span><p>{status?.dataConnections ?? status?.connectedWorkers ?? "—"}{status?.maxDataConnections != null ? ` / ${status.maxDataConnections}` : status?.configuredWorkers != null ? ` / ${status.configuredWorkers}` : ""}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Streams</span><p>{status?.activeStreams ?? "—"}{status?.streamCapacity != null ? ` / ${status.streamCapacity}` : ""}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Available slots</span><p>{status?.availableStreamSlots ?? "—"}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Queue</span><p>{status?.queuedRequests ?? "—"}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Desired data</span><p>{status?.desiredDataConnections ?? "—"}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Session</span><p class="truncate font-mono" title={status?.sessionId ?? ""}>{status?.sessionId ?? "—"}</p></div>
      <div><span class="text-[var(--color-text-muted)]">Route</span><p>{status?.routeId ?? "—"}{status?.sharedSession ? " · shared" : ""}</p></div>
    </div>
    <details class="mt-3 text-[11px] text-[var(--color-text-muted)]">
      <summary class="cursor-pointer">WSS identity / endpoint</summary>
      <div class="mt-2 grid gap-1 break-all font-mono">
        <div>origin: {status?.serverOrigin ?? "—"}</div>
        <div>wss: {status?.websocketUrl ?? "—"}</div>
        <div>server: {status?.serverId ?? "—"}</div>
        <div>device: {status?.deviceId ?? "—"}</div>
      </div>
    </details>
    {#if status?.lastError}
      <p class="mt-2 text-[11px] text-[var(--color-danger)]">{status.lastError}</p>
    {/if}
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
