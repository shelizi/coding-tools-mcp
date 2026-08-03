<script lang="ts">
  import { secretIsSet, setSecret, type SecretKey } from "$lib/api/secrets";
  import SecretInput from "$lib/components/SecretInput.svelte";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
    secretKey: SecretKey;
    label?: string;
    placeholder?: string;
    onSaved?: () => void;
    onValueChange?: (value: string) => void;
    hasPending?: boolean;
  }

  let {
    workspaceId,
    secretKey,
    label = "Cloudflare Tunnel Token",
    placeholder: customPlaceholder,
    onSaved,
    onValueChange,
    hasPending = $bindable(false),
  }: Props = $props();

  let draft = $state("");
  let saved = $state(false);
  let loading = $state(true);

  const placeholder = $derived(
    saved && !draft
      ? $t("Saved — click to update")
      : customPlaceholder ?? $t("Paste tunnel token"),
  );

  $effect(() => {
    const value = draft.trim();
    hasPending = value.length > 0;
    onValueChange?.(value);
  });

  $effect(() => {
    workspaceId;
    secretKey;
    void load();
  });

  async function load() {
    loading = true;
    try {
      draft = "";
      saved = await secretIsSet(workspaceId, secretKey);
    } finally {
      loading = false;
    }
  }

  export async function saveIfDirty(): Promise<boolean> {
    if (!draft.trim()) return false;
    await setSecret(workspaceId, secretKey, draft.trim());
    saved = true;
    draft = "";
    onSaved?.();
    return true;
  }

  export function hasPendingValue(): boolean {
    return hasPending;
  }

  export function pendingValue(): string {
    return draft.trim();
  }
</script>

<label class="grid gap-1">
  <span class="text-xs text-[var(--color-text-muted)]">{label}</span>
  <SecretInput
    bind:value={draft}
    {placeholder}
    disabled={loading}
    showCopy={false}
  />
</label>
