<script lang="ts">
  import { t } from "$lib/i18n";
  interface Props {
    name: string;
    onSave: (name: string) => void | Promise<void>;
  }

  let { name, onSave }: Props = $props();

  let draftName = $state("");
  let saving = $state(false);

  const dirty = $derived(draftName.trim() !== name && draftName.trim().length > 0);

  $effect(() => {
    draftName = name;
  });

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    try {
      await onSave(draftName.trim());
    } finally {
      saving = false;
    }
  }
</script>

<form
  class="flex flex-col gap-3 sm:flex-row sm:items-end"
  onsubmit={(event) => {
    event.preventDefault();
    void save();
  }}
>
  <label class="tx-field min-w-0 flex-1">
    <span class="tx-label">{$t("Workspace name")}</span>
    <input type="text" class="tx-input" bind:value={draftName} />
  </label>
  <button type="submit" class="tx-btn-primary shrink-0" disabled={saving || !dirty}>
    {saving ? $t("Saving…") : $t("Save name")}
  </button>
</form>
