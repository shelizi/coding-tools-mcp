import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";

const root = process.cwd();
const wizardPath = path.join(root, "src", "routes", "quick-setup", "+page.svelte");
const tunnelSetupPath = path.join(
  root,
  "src",
  "lib",
  "components",
  "quick-setup",
  "QuickTunnelSetup.svelte",
);

test("quick setup is an independent five-step route reachable from the app shell", async () => {
  const [wizard, layout, shell] = await Promise.all([
    readFile(wizardPath, "utf8"),
    readFile(path.join(root, "src", "routes", "+layout.svelte"), "utf8"),
    readFile(path.join(root, "src", "lib", "components", "AppShell.svelte"), "utf8"),
  ]);

  assert.match(
    wizard,
    /type WizardStep = "provider" \| "workspace" \| "service" \| "connect" \| "complete"/,
  );
  assert.match(wizard, /let step = \$state<WizardStep>\("provider"\)/);
  assert.match(layout, /goto\("\/quick-setup"\)/);
  assert.match(layout, /onQuickSetup=\{openQuickSetup\}/);
  assert.match(shell, /\$t\("Quick setup"\)/);
  assert.match(shell, /\$t\("Add workspace"\)/, "the original add-workspace entry remains available");
});

test("the first step offers all three supported reverse proxy sources", async () => {
  const wizard = await readFile(wizardPath, "utf8");

  assert.match(wizard, /type TunnelProvider = "builtin" \| "frp" \| "cloudflare"/);
  assert.match(wizard, /chooseProvider\("builtin"\)/);
  assert.match(wizard, /chooseProvider\("frp"\)/);
  assert.match(wizard, /chooseProvider\("cloudflare"\)/);
  assert.match(wizard, /\{ value: "provider", label: "Tunnel" \}/);
});

test("provider setup reuses managed software and global FRP profile contracts", async () => {
  const setup = await readFile(tunnelSetupPath, "utf8");

  assert.match(setup, /listSoftware\(\)/);
  assert.match(setup, /installSoftware\(softwareKind\)/);
  assert.match(setup, /provider === "frp" \? listFrpProfiles\(\)/);
  assert.match(setup, /saveFrpProfile\(/);
  assert.match(setup, /softwareKind = \$derived\(provider === "frp" \? "frpc" : "cloudflared"\)/);
  assert.match(setup, /cloudflareMode = \$state<CloudflareMode>\("quick"\)/);
  assert.match(setup, /Quick Tunnel/);
  assert.match(setup, /Named Tunnel/);
});

test("quick setup saves source-specific settings, tests the tunnel, and starts either service", async () => {
  const wizard = await readFile(wizardPath, "utf8");

  assert.match(wizard, /open\(\{ directory: true, multiple: true \}\)/);
  assert.match(wizard, /createWorkspace\(primaryFolder, workspaceName\.trim\(\) \|\| undefined\)/);
  assert.match(wizard, /for \(const folderPath of additionalFolders\)/);
  assert.match(wizard, /addWorkspaceFolder\(created\.id, folderPath\)/);
  assert.match(wizard, /setWorkspaceSecret\(workspaceId, key, value\)/);
  assert.match(wizard, /"builtin_tunnel_enrollment_url"/);
  assert.match(wizard, /"cloudflare_token" : "actions_cloudflare_token"/);
  assert.match(wizard, /\/clients\/\$\{workspaceId\}\/mcp/);
  assert.match(wizard, /frp_subdomain: frpSubdomain/);
  assert.match(wizard, /startRuntime\(workspaceId\)/);
  assert.match(wizard, /startActionsRuntime\(workspaceId\)/);
  assert.match(wizard, /<GptQuickCopy/);
});

test("MCP completion explains advanced OAuth, empty client secret, then connection", async () => {
  const [wizard, quickCopy] = await Promise.all([
    readFile(wizardPath, "utf8"),
    readFile(path.join(root, "src", "lib", "components", "GptQuickCopy.svelte"), "utf8"),
  ]);

  assert.match(wizard, /Paste the Public MCP endpoint shown here and choose OAuth authentication\./);
  assert.match(wizard, /Expand Advanced OAuth settings, enter the Client ID shown here, leave Client Secret empty, and keep the other OAuth settings at their defaults\./);
  assert.match(wizard, /Select Next, click Connect, then enter the one-time password shown here\./);
  assert.match(wizard, /guidedMcp=\{service === "mcp"\}/);
  assert.match(quickCopy, /\{#if !guidedMcp\}/);
  assert.match(quickCopy, /guidedMcp \? \$t\("One-time password"\)/);
});

test("quick setup starts the local service before testing and retaining the tunnel", async () => {
  const wizard = await readFile(wizardPath, "utf8");
  const enableIndex = wizard.indexOf("async function verifyAndEnable");
  const enableFlow = wizard.slice(enableIndex, wizard.indexOf("function goBack", enableIndex));
  const saveIndex = enableFlow.indexOf("await updateWorkspace(nextProfile)");
  const secretIndex = enableFlow.indexOf("await saveProviderSecret(workspaceId, targetService, input)");
  const verifyIndex = enableFlow.indexOf("await testTunnel(workspaceId, targetService)");
  const startIndex = enableFlow.indexOf("await startRuntime(workspaceId)");
  const refreshIndex = enableFlow.indexOf("await refreshWorkspaceStore(workspaceId)");
  const completeIndex = enableFlow.indexOf('step = "complete"');

  assert.ok(saveIndex >= 0 && saveIndex < secretIndex);
  assert.ok(secretIndex < startIndex);
  assert.ok(startIndex < verifyIndex);
  assert.match(enableFlow, /!tunnel\.keptRunning/);
  assert.ok(verifyIndex < refreshIndex);
  assert.ok(refreshIndex < completeIndex);
});
