<script lang="ts">
  import ChatGptSessionPrompt from "$lib/components/ChatGptSessionPrompt.svelte";
  import WorkspaceFolderManager from "$lib/components/WorkspaceFolderManager.svelte";
  import WorkspaceMetaForm from "$lib/components/WorkspaceMetaForm.svelte";
  import { t } from "$lib/i18n";
  import type { WorkspaceProfile } from "$lib/types";

  interface Props {
    profile: WorkspaceProfile;
    onSaveName: (name: string) => void | Promise<void>;
    onProfileChanged: (profile: WorkspaceProfile) => void | Promise<void>;
    onFoldersChanged: () => void | Promise<void>;
  }

  let { profile, onSaveName, onProfileChanged, onFoldersChanged }: Props = $props();
</script>

<div class="grid gap-5">
  <section class="tx-card p-5">
    <p class="tx-section-label">{$t("Basic information")}</p>
    <div class="mt-3">
      <WorkspaceMetaForm name={profile.name} onSave={onSaveName} />
    </div>
  </section>

  <section class="tx-card p-5">
    <p class="tx-section-label">{$t("Project folders")}</p>
    <div class="mt-3">
      <WorkspaceFolderManager
        {profile}
        onChanged={onProfileChanged}
        {onFoldersChanged}
      />
    </div>
  </section>

  <section class="tx-card p-5">
    <p class="tx-section-label">{$t("ChatGPT session")}</p>
    <div class="mt-3">
      <ChatGptSessionPrompt />
    </div>
  </section>
</div>
