<script lang="ts">
  import FolderOpen from "@lucide/svelte/icons/folder-open";
  import FolderPlus from "@lucide/svelte/icons/folder-plus";
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import { confirm, open } from "@tauri-apps/plugin-dialog";
  import {
    addWorkspaceFolder,
    openWorkspaceDirectory,
    removeWorkspaceFolder,
  } from "$lib/api/workspaces";
  import { showToast } from "$lib/stores/toast";
  import { workspaceFolders, type WorkspaceProfile } from "$lib/types";
  import { t } from "$lib/i18n";

  interface Props {
    profile: WorkspaceProfile;
    onChanged: (profile: WorkspaceProfile) => void | Promise<void>;
    onFoldersChanged?: (profile: WorkspaceProfile) => void | Promise<void>;
  }

  let { profile, onChanged, onFoldersChanged }: Props = $props();
  let busyAction = $state("");

  const folders = $derived(workspaceFolders(profile));

  function normalizePath(value: string): string {
    const trimmed = value.trim();
    if (/^[A-Za-z]:[\\/]$/.test(trimmed) || trimmed === "/") return trimmed;
    return trimmed.replace(/[\\/]+$/, "");
  }

  async function chooseFolder() {
    if (busyAction) return;
    busyAction = "add";
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        defaultPath: profile.path || undefined,
      });
      if (!selected || Array.isArray(selected)) return;
      const path = normalizePath(selected);
      if (!path) return;
      const updated = await addWorkspaceFolder(profile.id, path);
      await onChanged(updated);
      await onFoldersChanged?.(updated);
      showToast($t("Folder added to the workspace. MCP stays connected; running Actions will prompt for a restart to refresh the manifest."), { kind: "success" });
    } catch (error) {
      showToast(String(error), { kind: "error", title: $t("Could not add folder") });
    } finally {
      busyAction = "";
    }
  }

  async function openFolder(path: string, folderId: string) {
    if (busyAction) return;
    busyAction = `open:${folderId}`;
    try {
      await openWorkspaceDirectory(path);
    } catch (error) {
      showToast(String(error), { kind: "error", title: $t("Could not open directory") });
    } finally {
      busyAction = "";
    }
  }

  async function removeFolder(folderId: string, folderName: string) {
    if (busyAction || folders.length <= 1) return;
    const confirmed = await confirm(
      $t("Remove folder “{name}” from the workspace? Files on disk will not be deleted.", { name: folderName }),
      {
        title: $t("Remove folder"),
        kind: "warning",
        okLabel: $t("Remove"),
        cancelLabel: $t("Cancel"),
      },
    );
    if (!confirmed) return;

    busyAction = `remove:${folderId}`;
    try {
      const updated = await removeWorkspaceFolder(profile.id, folderId);
      await onChanged(updated);
      await onFoldersChanged?.(updated);
      showToast($t("Folder removed from the workspace. Restart running Actions to refresh the manifest."), { kind: "success" });
    } catch (error) {
      showToast(String(error), { kind: "error", title: $t("Could not remove folder") });
    } finally {
      busyAction = "";
    }
  }
</script>

<section class="tx-card p-4">
  <div class="flex flex-wrap items-start justify-between gap-3">
    <div>
      <h3 class="text-sm font-semibold text-[var(--color-text-primary)]">{$t("Folder list")}</h3>
      <p class="mt-1 max-w-3xl text-xs leading-5 text-[var(--color-text-muted)]">
        {$t("All folders share the same MCP service, authentication, port, and tunnel.")}
        {$t("Each folder keeps independent tool context and conversation history under")}
        <span class="tx-mono">docs/history-session</span>.
        {$t("MCP and Actions require an explicit folder selection; no default folder is used.")}
      </p>
    </div>
    <button
      type="button"
      class="tx-btn-primary shrink-0"
      disabled={Boolean(busyAction)}
      onclick={() => void chooseFolder()}
    >
      <FolderPlus size={15} class="inline-block" />
      <span class="ml-1">{busyAction === "add" ? $t("Selecting…") : $t("Add folder")}</span>
    </button>
  </div>

  <div class="mt-4 grid gap-2">
    {#each folders as folder (folder.id)}
      <article class="rounded-[12px] border border-[var(--color-border)] px-3 py-3">
        <div class="flex min-w-0 flex-wrap items-center justify-between gap-3">
          <div class="min-w-0 flex-1">
            <span class="block truncate text-sm font-medium text-[var(--color-text-primary)]">
              {folder.name}
            </span>
            <p class="tx-mono mt-1 truncate text-xs text-[var(--color-text-muted)]" title={folder.path}>
              {folder.path}
            </p>
          </div>

          <div class="flex shrink-0 flex-wrap items-center gap-1.5">
            <button
              type="button"
              class="tx-btn-ghost px-2.5 py-1.5 text-xs"
              disabled={Boolean(busyAction)}
              onclick={() => void openFolder(folder.path, folder.id)}
            >
              <FolderOpen size={14} class="inline-block" />
              <span class="ml-1">{$t("Open")}</span>
            </button>
            <button
              type="button"
              class="tx-btn-ghost px-2.5 py-1.5 text-xs text-[var(--danger)]"
              disabled={Boolean(busyAction) || folders.length <= 1}
              title={folders.length <= 1 ? $t("A workspace must keep at least one folder") : $t("Remove from workspace")}
              onclick={() => void removeFolder(folder.id, folder.name)}
            >
              <Trash2 size={14} class="inline-block" />
              <span class="ml-1">{$t("Remove")}</span>
            </button>
          </div>
        </div>
      </article>
    {/each}
  </div>
</section>
