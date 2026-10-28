<script lang="ts">
  import { onMount } from "svelte";
  import {
    formatRemaining,
    getSessionExpiry,
    remainingMs,
    type SessionExpirySnapshot,
  } from "$lib/api/session-expiry";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
  }

  let { workspaceId }: Props = $props();
  let snapshot = $state<SessionExpirySnapshot | null>(null);
  let now = $state(Date.now());
  let error = $state("");
  let requestSequence = 0;

  async function reload(id: string) {
    const sequence = ++requestSequence;
    try {
      const result = await getSessionExpiry(id);
      if (sequence !== requestSequence || id !== workspaceId) return;
      snapshot = result;
      error = "";
    } catch (err) {
      if (sequence !== requestSequence || id !== workspaceId) return;
      snapshot = null;
      error = String(err);
    }
  }

  $effect(() => {
    const id = workspaceId;
    snapshot = null;
    error = "";
    void reload(id);
    return () => { requestSequence += 1; };
  });

  onMount(() => {
    const clockTimer = setInterval(() => { now = Date.now(); }, 1000);
    const refreshTimer = setInterval(() => { void reload(workspaceId); }, 15_000);
    return () => {
      clearInterval(clockTimer);
      clearInterval(refreshTimer);
      requestSequence += 1;
    };
  });

  function warningLevel(expiresAt: number): "expired" | "soon" | "valid" {
    const remaining = remainingMs(expiresAt, now);
    return remaining <= 0 ? "expired" : remaining <= 15 * 60_000 ? "soon" : "valid";
  }
</script>

<section class="tx-card p-5" aria-label={$t("Session expiration")}>
  <div class="flex flex-wrap items-start justify-between gap-2">
    <div>
      <h3 class="font-semibold">{$t("Session expiration")}</h3>
      <p class="mt-1 text-sm text-[var(--color-text-muted)]">
        {$t("Countdowns for known credentials in this running MCP process.")}
      </p>
    </div>
    {#if snapshot?.runtimeActive}
      <span class="tx-status-pill">{$t("Local runtime")}</span>
    {/if}
  </div>

  {#if error}
    <p role="alert" class="mt-3 text-sm text-[var(--color-error)]">
      {$t("Unable to load session expiration data.")} {error}
    </p>
  {:else if !snapshot}
    <p class="mt-3 text-sm text-[var(--color-text-muted)]">{$t("Loading…")}</p>
  {:else if !snapshot.runtimeActive}
    <p class="mt-3 text-sm text-[var(--color-text-muted)]">
      {$t("MCP service is not running; no active expiry data is available.")}
    </p>
  {:else}
    <div class="mt-4 grid gap-4 md:grid-cols-2">
      {#if snapshot.authType === "oauth"}
        <div class="min-w-0 rounded-lg bg-[var(--color-bg)] p-3">
          <div class="flex justify-between gap-2">
            <h4 class="text-sm font-semibold">{$t("Observed OAuth tokens")}</h4>
            <span class="text-xs text-[var(--color-text-muted)]">{snapshot.oauthTokenExpiresAtMs.length}</span>
          </div>
          {#if snapshot.oauthTokenTtlSeconds != null}
            <p class="mt-1 text-xs text-[var(--color-text-muted)]">
              {$t("Configured token lifetime")}: {formatRemaining(now + snapshot.oauthTokenTtlSeconds * 1000, now)}
            </p>
          {/if}
          {#if snapshot.oauthTokenExpiresAtMs.length === 0}
            <p class="mt-3 text-sm text-[var(--color-text-muted)]">{$t("No credentials observed yet.")}</p>
          {:else}
            <ul class="mt-3 grid gap-2">
              {#each snapshot.oauthTokenExpiresAtMs.slice(0, 8) as expiry, index (`${expiry}-${index}`)}
                <li class="flex items-start justify-between gap-2 text-sm">
                  <span class="text-[var(--color-text-muted)]">Token {index + 1}</span>
                  <div class="text-right">
                    <p class:warning={warningLevel(expiry) === "soon"} class:expired={warningLevel(expiry) === "expired"}
                      class="font-mono tabular-nums">{formatRemaining(expiry, now)}</p>
                    <p class="text-xs text-[var(--color-text-muted)]" title={new Date(expiry).toLocaleString()}>
                      {new Date(expiry).toLocaleString()}
                    </p>
                  </div>
                </li>
              {/each}
            </ul>
          {/if}
        </div>
      {/if}
      <div class="min-w-0 rounded-lg bg-[var(--color-bg)] p-3">
        <div class="flex justify-between gap-2">
          <h4 class="text-sm font-semibold">{$t("Conversation handles")}</h4>
          <span class="text-xs text-[var(--color-text-muted)]">{snapshot.conversationHandleExpiresAtMs.length}</span>
        </div>
        <p class="mt-1 text-xs text-[var(--color-text-muted)]">{$t("Handles expire after 24 hours or when the server restarts.")}</p>
        {#if snapshot.conversationHandleExpiresAtMs.length === 0}
          <p class="mt-3 text-sm text-[var(--color-text-muted)]">{$t("No credentials observed yet.")}</p>
        {:else}
          <ul class="mt-3 grid gap-2">
            {#each snapshot.conversationHandleExpiresAtMs.slice(0, 8) as expiry, index (`${expiry}-${index}`)}
              <li class="flex items-start justify-between gap-2 text-sm">
                <span class="text-[var(--color-text-muted)]">Handle {index + 1}</span>
                <div class="text-right">
                  <p class:warning={warningLevel(expiry) === "soon"} class:expired={warningLevel(expiry) === "expired"}
                    class="font-mono tabular-nums">{formatRemaining(expiry, now)}</p>
                  <p class="text-xs text-[var(--color-text-muted)]">{new Date(expiry).toLocaleString()}</p>
                </div>
              </li>
            {/each}
          </ul>
        {/if}
      </div>
    </div>
    <p class="mt-3 text-xs text-[var(--color-text-muted)]">
      {$t("Only credentials issued or verified by this process are shown. Restarting clears the observed list; no token or handle values are exposed.")}
    </p>
  {/if}
</section>

<style>
  .warning { color: var(--color-warning); }
  .expired { color: var(--color-error); }
</style>
