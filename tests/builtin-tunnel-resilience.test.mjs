import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";

const root = process.cwd();

async function source(...segments) {
  return readFile(path.join(root, ...segments), "utf8");
}

test("built-in tunnel worker policy is server-managed", async () => {
  const form = await source("src", "lib", "components", "TunnelConfigForm.svelte");
  const mcpPanel = await source(
    "src",
    "lib",
    "components",
    "workspace",
    "McpWorkspacePanel.svelte",
  );
  const actionsPanel = await source(
    "src",
    "lib",
    "components",
    "workspace",
    "ActionsWorkspacePanel.svelte",
  );
  const workspacePage = await source("src", "routes", "workspace", "[id]", "+page.svelte");
  const types = await source("src", "lib", "types.ts");

  for (const contents of [form, mcpPanel, actionsPanel, workspacePage, types]) {
    assert.doesNotMatch(contents, /builtin_worker_count/);
  }
});

test("tunnel status exposes live worker telemetry", async () => {
  const api = await source("src", "lib", "api", "tunnel.ts");
  assert.match(api, /configuredWorkers:\s*number \| null/);
  assert.match(api, /connectedWorkers:\s*number \| null/);
  assert.match(api, /idleWorkers:\s*number \| null/);
  assert.match(api, /busyWorkers:\s*number \| null/);
  assert.match(api, /recycledWorkers:\s*number \| null/);
  assert.match(api, /policyRevision:\s*number \| null/);
  assert.match(api, /lastError:\s*string \| null/);
});

test("saving a built-in enrollment resolves and reloads the server-assigned public URL", async () => {
  const workspacePage = await source("src", "routes", "workspace", "[id]", "+page.svelte");
  const form = await source("src", "lib", "components", "TunnelConfigForm.svelte");

  assert.match(workspacePage, /import \{ restartTunnel, stopTunnel, testTunnel \}/);
  assert.match(workspacePage, /config\.type === "builtin"[\s\S]*testTunnel\(targetWorkspaceId, service\)/);
  assert.match(workspacePage, /restartTunnelIfConfigured\(targetWorkspaceId, config, "mcp"\);\s*await refreshProfile\(targetWorkspaceId\)/);
  assert.match(workspacePage, /restartTunnelIfConfigured\(targetWorkspaceId, config, "actions"\);\s*await refreshProfile\(targetWorkspaceId\)/);
  assert.match(form, /draft = \{ \.\.\.draft, public_url: result\.publicUrl \};\s*await onSave\(draft, \{ skipTunnelRestart: true, skipServicePrompt: true \}\)/);
});
