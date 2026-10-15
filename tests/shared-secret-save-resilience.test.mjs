import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const authFormPath = new URL("../src/lib/components/AuthConfigForm.svelte", import.meta.url);
const actionsAuthFormPath = new URL(
  "../src/lib/components/ActionsAuthForm.svelte",
  import.meta.url,
);
const mcpPanelPath = new URL(
  "../src/lib/components/workspace/McpWorkspacePanel.svelte",
  import.meta.url,
);
const workspacePagePath = new URL(
  "../src/routes/workspace/[id]/+page.svelte",
  import.meta.url,
);

test("saving shared MCP OAuth credentials schedules only one runtime restart", async () => {
  const [authForm, mcpPanel, workspacePage] = await Promise.all([
    readFile(authFormPath, "utf8"),
    readFile(mcpPanelPath, "utf8"),
    readFile(workspacePagePath, "utf8"),
  ]);

  assert.match(authForm, /sharedSecretChanged = clientId !== loadedSharedOauthClientId/);
  assert.match(authForm, /skipRuntimeRestart: sharedSecretChanged/);
  assert.match(authForm, /if \(sharedSecretChanged\) \{\s*await setSharedSecret/);
  assert.ok(
    authForm.indexOf("await onSaveProfile") < authForm.indexOf('await setSharedSecret("oauth_client_id"'),
    "profile flags must be persisted before a shared-secret change schedules a backend restart",
  );

  assert.match(mcpPanel, /options\?: SaveAuthOptions/);
  assert.match(
    workspacePage,
    /if \(!options\?\.skipRuntimeRestart && mcpStatus === "running"\)/,
  );
});

test("regenerated secrets are already saved and cannot enable a second restart", async () => {
  const [authForm, actionsAuthForm] = await Promise.all([
    readFile(authFormPath, "utf8"),
    readFile(actionsAuthFormPath, "utf8"),
  ]);

  assert.match(authForm, /loadedSecrets = \{ \.\.\.loadedSecrets, \[key\]: value \}/);
  assert.match(actionsAuthForm, /loadedApiKey = apiKey/);
  assert.match(actionsAuthForm, /loadedOauthClientSecret = oauthClientSecret/);
  assert.match(actionsAuthForm, /loadedOauthPassword = oauthPassword/);
  assert.match(actionsAuthForm, /loadedOauthTokenSecret = oauthTokenSecret/);
});

test("Node-host MCP authorization password can be edited and saved through the workspace secret API", async () => {
  const authForm = await readFile(authFormPath, "utf8");

  assert.match(authForm, /setWorkspaceSecret,/);
  assert.match(authForm, /const canEditWorkspaceOauthPassword = \$derived\(capabilities\.host === "node"\)/);
  assert.match(
    authForm,
    /workspaceOauthPasswordChanged[\s\S]*capabilities\.host === "node"[\s\S]*secrets\.oauth_password[\s\S]*loadedSecrets\.oauth_password/,
  );
  assert.match(
    authForm,
    /await setWorkspaceSecret\(workspaceId, "oauth_password", oauthPassword\)/,
  );
  assert.match(
    authForm,
    /bind:value=\{secrets\.oauth_password\}[\s\S]*readonly=\{!canEditWorkspaceOauthPassword\}/,
  );
});
