<script lang="ts">
  import { t } from "$lib/i18n";

  interface Props {
    open: boolean;
    distributions: string[];
    busy?: boolean;
    error?: string;
    onClose: () => void;
    onSubmit: (distro: string, linuxPath: string, name?: string) => void | Promise<void>;
  }

  let { open, distributions, busy = false, error = "", onClose, onSubmit }: Props = $props();
  let distro = $state("");
  let linuxPath = $state("");
  let name = $state("");

  $effect(() => {
    if (open && !distributions.includes(distro)) {
      distro = distributions[0] ?? "";
    }
  });

  function close() {
    if (!busy) onClose();
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (!distro || !linuxPath.trim() || busy) return;
    await onSubmit(distro, linuxPath.trim(), name.trim() || undefined);
  }

  function backdropClick(event: MouseEvent) {
    if (event.target === event.currentTarget) close();
  }
</script>

<svelte:window onkeydown={(event) => event.key === "Escape" && open && close()} />

{#if open}
  <div class="tx-dialog-backdrop" role="presentation" onclick={backdropClick}>
    <dialog
      open
      class="tx-dialog"
      aria-modal="true"
      aria-labelledby="wsl-folder-title"
    >
      <div>
        <h2 id="wsl-folder-title" class="text-base font-semibold">{$t("Add WSL folder")}</h2>
        <p class="mt-1 text-xs text-[var(--color-text-muted)]">
          {$t("Commands run inside WSL while the desktop client accesses files through the WSL share.")}
        </p>
      </div>

      <form class="mt-5 grid gap-4" onsubmit={submit}>
        <label class="tx-field">
          <span class="tx-label">{$t("WSL distribution")}</span>
          <select class="tx-select" bind:value={distro} disabled={busy}>
            {#each distributions as distribution}
              <option value={distribution}>{distribution}</option>
            {/each}
          </select>
        </label>

        <label class="tx-field">
          <span class="tx-label">{$t("Linux folder path")}</span>
          <input
            class="tx-input tx-mono"
            bind:value={linuxPath}
            placeholder="/opt/src/SampleProject"
            autocomplete="off"
            disabled={busy}
          />
        </label>

        <label class="tx-field">
          <span class="tx-label">{$t("Folder name (optional)")}</span>
          <input class="tx-input" bind:value={name} autocomplete="off" disabled={busy} />
        </label>

        {#if error}
          <div class="tx-alert tx-alert--error">{error}</div>
        {/if}

        <div class="flex justify-end gap-2">
          <button type="button" class="tx-btn-ghost" onclick={close} disabled={busy}>
            {$t("Cancel")}
          </button>
          <button type="submit" class="tx-btn-primary" disabled={busy || !distro || !linuxPath.trim()}>
            {busy ? $t("Adding…") : $t("Add folder")}
          </button>
        </div>
      </form>
    </dialog>
  </div>
{/if}
