<script lang="ts">
  import { t, type MessageKey } from "$lib/i18n";
  export interface RuntimePolicyDraft {
    transportMode: string;
    toolProfile: string;
    permissionMode: string;
    allowedCommands: string;
    workspaceLocalEntries: boolean;
    workspaceScriptExtensions: string;
    blockingAdmissionLimit: number;
    processAdmissionLimit: number;
    globalBlockingAdmissionLimit: number;
    globalProcessAdmissionLimit: number;
    activeSessionLimit: number;
  }

  interface Props {
    transportMode: string;
    toolProfile: string;
    permissionMode: string;
    allowedCommands: string;
    workspaceLocalEntries: boolean;
    workspaceScriptExtensions: string;
    blockingAdmissionLimit: number;
    processAdmissionLimit: number;
    globalBlockingAdmissionLimit: number;
    globalProcessAdmissionLimit: number;
    activeSessionLimit: number;
    onSave: (draft: RuntimePolicyDraft) => void | Promise<void>;
  }

  const TOOL_PROFILE_OPTIONS = [
    { value: "full", label: "Full tools" },
    { value: "read-only", label: "Read-only tools" },
    { value: "compat-readonly-all", label: "Compatibility read-only" },
  ] as const;

  const TRANSPORT_MODE_OPTIONS = [
    { value: "streamable-http", label: "Standard Streamable HTTP (recommended)" },
    { value: "legacy-json", label: "Legacy JSON compatibility mode" },
  ] as const;

  const PERMISSION_MODE_OPTIONS = [
    { value: "trusted", label: "Trusted" },
    { value: "safe", label: "Restricted" },
    { value: "dangerous", label: "Unrestricted" },
  ] as const;

  let { transportMode, toolProfile, permissionMode, allowedCommands, workspaceLocalEntries, workspaceScriptExtensions, blockingAdmissionLimit, processAdmissionLimit, globalBlockingAdmissionLimit, globalProcessAdmissionLimit, activeSessionLimit, onSave }: Props = $props();

  let draftTransportMode = $state("streamable-http");
  let draftProfile = $state("full");
  let draftMode = $state("trusted");
  let draftCommands = $state("");
  let draftLocalEntries = $state(true);
  let draftExtensions = $state(".exe,.bat,.cmd,.ps1");
  let draftBlockingLimit = $state(128);
  let draftProcessLimit = $state(64);
  let draftGlobalBlockingLimit = $state(1024);
  let draftGlobalProcessLimit = $state(512);
  let draftActiveSessions = $state(512);
  let saving = $state(false);

  const dirty = $derived(
    draftTransportMode !== transportMode || draftProfile !== toolProfile || draftMode !== permissionMode || draftCommands !== allowedCommands || draftLocalEntries !== workspaceLocalEntries || draftExtensions !== workspaceScriptExtensions || draftBlockingLimit !== blockingAdmissionLimit || draftProcessLimit !== processAdmissionLimit || draftGlobalBlockingLimit !== globalBlockingAdmissionLimit || draftGlobalProcessLimit !== globalProcessAdmissionLimit || draftActiveSessions !== activeSessionLimit,
  );

  $effect(() => {
    draftTransportMode = transportMode;
    draftProfile = toolProfile;
    draftMode = permissionMode;
    draftCommands = allowedCommands;
    draftLocalEntries = workspaceLocalEntries;
    draftExtensions = workspaceScriptExtensions;
    draftBlockingLimit = blockingAdmissionLimit;
    draftProcessLimit = processAdmissionLimit;
    draftGlobalBlockingLimit = globalBlockingAdmissionLimit;
    draftGlobalProcessLimit = globalProcessAdmissionLimit;
    draftActiveSessions = activeSessionLimit;
  });

  async function save() {
    if (saving || !dirty) return;
    saving = true;
    try {
      await onSave({ transportMode: draftTransportMode, toolProfile: draftProfile, permissionMode: draftMode, allowedCommands: draftCommands.trim(), workspaceLocalEntries: draftLocalEntries, workspaceScriptExtensions: draftExtensions.trim(), blockingAdmissionLimit: Math.min(65535, Math.max(1, draftBlockingLimit)), processAdmissionLimit: Math.min(65535, Math.max(1, draftProcessLimit)), globalBlockingAdmissionLimit: Math.min(65535, Math.max(1, draftGlobalBlockingLimit)), globalProcessAdmissionLimit: Math.min(65535, Math.max(1, draftGlobalProcessLimit)), activeSessionLimit: Math.min(65535, Math.max(1, draftActiveSessions)) });
    } finally {
      saving = false;
    }
  }
</script>

<form
  class="grid gap-3"
  onsubmit={(event) => {
    event.preventDefault();
    void save();
  }}
>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">{$t("HTTP transport mode")}</span>
    <select
      class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draftTransportMode}
    >
      {#each TRANSPORT_MODE_OPTIONS as option}
        <option value={option.value}>{$t(option.label as MessageKey)}</option>
      {/each}
    </select>
    <span class="text-xs text-[var(--color-text-muted)]">
      {$t("Standard mode supports ChatGPT and current MCP clients. Use legacy mode only to preserve older connection behavior.")}
    </span>
  </label>
  <div class="grid grid-cols-1 gap-3 sm:grid-cols-3">
    <label class="grid gap-1">
      <span class="text-xs text-[var(--color-text-muted)]">{$t("Workspace blocking concurrency")}</span>
      <input type="number" min="1" max="65535" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftBlockingLimit} />
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--color-text-muted)]">{$t("Workspace process concurrency")}</span>
      <input type="number" min="1" max="65535" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftProcessLimit} />
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--color-text-muted)]">{$t("Active command session limit")}</span>
      <input type="number" min="1" max="65535" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftActiveSessions} />
    </label>
  </div>
  <div class="grid grid-cols-1 gap-3 sm:grid-cols-2">
    <label class="grid gap-1">
      <span class="text-xs text-[var(--color-text-muted)]">{$t("Global blocking concurrency")}</span>
      <input type="number" min="1" max="65535" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftGlobalBlockingLimit} />
    </label>
    <label class="grid gap-1">
      <span class="text-xs text-[var(--color-text-muted)]">{$t("Global process concurrency")}</span>
      <input type="number" min="1" max="65535" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm" bind:value={draftGlobalProcessLimit} />
    </label>
  </div>
  <span class="text-xs text-[var(--color-text-muted)]">{$t("Defaults are intentionally generous; 65535 is only the configuration format boundary. Workspace capacity is acquired before global capacity to prevent one workspace from occupying the global queue.")}</span>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">{$t("Tool profile")}</span>
    <select
      class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draftProfile}
    >
      {#each TOOL_PROFILE_OPTIONS as option}
        <option value={option.value}>{$t(option.label as MessageKey)}</option>
      {/each}
    </select>
  </label>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">{$t("System commands (comma separated)")}</span>
    <input type="text" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 font-mono text-sm" placeholder="python,git,curl,powershell,..." bind:value={draftCommands} />
  </label>
  <label class="flex items-center gap-2 text-sm">
    <input type="checkbox" bind:checked={draftLocalEntries} />
    <span>{$t("Allow local workspace entry points")}</span>
  </label>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">{$t("Local script extensions (comma separated)")}</span>
    <input type="text" class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 font-mono text-sm" placeholder=".exe,.bat,.cmd,.ps1" bind:value={draftExtensions} disabled={!draftLocalEntries} />
  </label>
  <label class="grid gap-1">
    <span class="text-xs text-[var(--color-text-muted)]">{$t("Permission mode")}</span>
    <select
      class="rounded-md border border-[var(--color-border)] bg-[var(--color-bg)] px-2.5 py-1.5 text-sm"
      bind:value={draftMode}
    >
      {#each PERMISSION_MODE_OPTIONS as option}
        <option value={option.value}>{$t(option.label as MessageKey)}</option>
      {/each}
    </select>
  </label>
  <p class="text-xs text-[var(--color-text-muted)]">
    {$t("Local workspace entry points resolve from the current working directory. System commands and script types are configurable per project. The execution boundary remains policy_only.")}
  </p>
  <div class="flex justify-end pt-1">
    <button
      type="submit"
      class="rounded-md bg-[var(--color-accent)] px-3 py-1.5 text-sm font-medium text-white disabled:opacity-50"
      disabled={saving || !dirty}
    >
      {saving ? $t("Saving…") : $t("Save policy")}
    </button>
  </div>
</form>
