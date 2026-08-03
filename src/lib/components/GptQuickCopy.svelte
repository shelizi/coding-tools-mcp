<script lang="ts">
  import CopyFieldRow from "$lib/components/CopyFieldRow.svelte";
  import { getSecret, getSharedSecret } from "$lib/api/secrets";
  import type { AuthConfig, WorkspaceProfile } from "$lib/types";
  import {
    actionsOAuthAuthorizeUrl,
    actionsOAuthTokenUrl,
    actionsOpenApiUrl,
    actionsPrivacyUrl,
    actionsConfig,
  } from "$lib/types";
  import { t } from "$lib/i18n";

  interface Props {
    workspaceId: string;
    service: "mcp" | "actions";
    profile: WorkspaceProfile;
    publicMcpEndpoint?: string;
    frpProfiles?: { id: string; name: string; server: string; serverPort: number }[];
    guidedMcp?: boolean;
  }

  let {
    workspaceId,
    service,
    profile,
    publicMcpEndpoint = "",
    frpProfiles = [],
    guidedMcp = false,
  }: Props = $props();

  let loading = $state(true);
  let secrets = $state<Record<string, string>>({});

  const actions = $derived(actionsConfig(profile));
  const auth = $derived(profile.auth);

  async function loadSecrets() {
    loading = true;
    try {
      if (service === "mcp") {
        const useShared = auth.use_shared_secrets ?? false;
        const fetchSecret = async (key: string, sharedKey: string) => {
          const value = useShared
            ? await getSharedSecret(sharedKey as Parameters<typeof getSharedSecret>[0])
            : await getSecret(workspaceId, key as Parameters<typeof getSecret>[1]);
          return value ?? "";
        };
        if (auth.type === "oauth") {
          const clientId = useShared
            ? ((await getSharedSecret("oauth_client_id")) ?? "")
            : auth.oauth_client_id;
          secrets = {
            oauth_client_id: clientId,
            oauth_client_secret: await fetchSecret("oauth_client_secret", "oauth_client_secret"),
            oauth_password: await fetchSecret("oauth_password", "oauth_password"),
          };
        } else if (auth.type === "bearer") {
          secrets = {
            bearer_token: await fetchSecret("bearer_token", "bearer_token"),
          };
        } else {
          secrets = {};
        }
      } else {
        const useShared = actions.use_shared_secrets ?? false;
        const fetchSecret = async (key: string, sharedKey: string) => {
          const value = useShared
            ? await getSharedSecret(sharedKey as Parameters<typeof getSharedSecret>[0])
            : await getSecret(workspaceId, key as Parameters<typeof getSecret>[1]);
          return value ?? "";
        };
        if (actions.auth_type === "api_key") {
          secrets = { actions_api_key: await fetchSecret("actions_api_key", "actions_api_key") };
        } else if (actions.auth_type === "oauth") {
          secrets = {
            actions_oauth_client_secret: await fetchSecret(
              "actions_oauth_client_secret",
              "actions_oauth_client_secret",
            ),
          };
        } else {
          secrets = {};
        }
      }
    } finally {
      loading = false;
    }
  }

  $effect(() => {
    workspaceId;
    service;
    auth.type;
    auth.oauth_client_id;
    auth.use_shared_secrets;
    actions.auth_type;
    actions.oauth_client_id;
    actions.oauth_scopes;
    actions.use_shared_secrets;
    void loadSecrets();
  });
</script>

<article class="tx-card p-5">
  <div class="mb-4">
    <p class="tx-section-label">{$t("GPT configuration")}</p>
    <p class="mt-1 text-xs text-[var(--color-text-muted)]">
      {service === "mcp"
        ? $t("Copy these values to ChatGPT → Settings → Connectors / MCP")
        : $t("Copy these values to the GPT editor → Actions")}
    </p>
  </div>

  <div class="grid gap-3">
    {#if service === "mcp"}
      <CopyFieldRow
        label={$t("Public MCP endpoint")}
        value={publicMcpEndpoint}
        hint={$t("Enter this URL in the GPT connector")}
      />
      {#if auth.type === "oauth"}
        <CopyFieldRow label="OAuth Client ID" value={secrets.oauth_client_id ?? auth.oauth_client_id} {loading} />
        {#if !guidedMcp}
          <CopyFieldRow
            label="OAuth Client Secret"
            value={secrets.oauth_client_secret ?? ""}
            {loading}
          />
        {/if}
        <CopyFieldRow
          label={guidedMcp ? $t("One-time password") : $t("Authorization password")}
          value={secrets.oauth_password ?? ""}
          hint={guidedMcp
            ? $t("Enter this after clicking Connect in ChatGPT")
            : $t("Enter when ChatGPT authorizes for the first time")}
          {loading}
        />
      {:else if auth.type === "bearer"}
        <CopyFieldRow label="Bearer Token" value={secrets.bearer_token ?? ""} {loading} />
      {:else}
        <p class="text-xs text-[var(--color-text-muted)]">{$t("Authentication is disabled; use for local debugging only.")}</p>
      {/if}
    {:else}
      <CopyFieldRow
        label="OpenAPI Schema URL"
        value={actionsOpenApiUrl(profile, frpProfiles)}
        hint="Actions → Import from URL"
      />
      <CopyFieldRow
        label={$t("Privacy policy URL")}
        value={actionsPrivacyUrl(profile, frpProfiles)}
        hint={$t("GPT Actions privacy policy field")}
      />
      {#if actions.auth_type === "api_key"}
        <CopyFieldRow
          label={$t("API Key (Bearer)")}
          value={secrets.actions_api_key ?? ""}
          hint={$t("Choose API Key → Bearer for Actions authentication")}
          {loading}
        />
      {:else if actions.auth_type === "oauth"}
        <CopyFieldRow label="OAuth Client ID" value={actions.oauth_client_id ?? ""} />
        <CopyFieldRow
          label="OAuth Client Secret"
          value={secrets.actions_oauth_client_secret ?? ""}
          {loading}
        />
        <CopyFieldRow
          label="Authorization URL"
          value={actionsOAuthAuthorizeUrl(profile, frpProfiles)}
        />
        <CopyFieldRow label="Token URL" value={actionsOAuthTokenUrl(profile, frpProfiles)} />
        <CopyFieldRow label="Scope" value={actions.oauth_scopes ?? ""} hint={$t("Space separated")} />
      {:else}
        <p class="text-xs text-[var(--color-text-muted)]">{$t("Authentication is disabled; use API Key or OAuth for public access.")}</p>
      {/if}
    {/if}
  </div>
</article>
