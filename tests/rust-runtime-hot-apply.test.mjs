import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const workspacePagePath = new URL(
  "../src/routes/workspace/[id]/+page.svelte",
  import.meta.url,
);

test("MCP tool and command policy changes hot-apply without a restart prompt", async () => {
  const page = await readFile(workspacePagePath, "utf8");
  const start = page.indexOf("async function saveMcpPolicy");
  const end = page.indexOf("async function saveActionsPolicy", start);
  assert.ok(start >= 0 && end > start, "saveMcpPolicy function should be present");
  const savePolicy = page.slice(start, end);
  const restartStart = savePolicy.indexOf("const requiresRestart");
  const nextStart = savePolicy.indexOf("const next", restartStart);
  assert.ok(restartStart >= 0 && nextStart > restartStart, "restart predicate should be present");
  const restartPredicate = savePolicy.slice(restartStart, nextStart);

  for (const field of [
    "transportMode",
    "blockingAdmissionLimit",
    "processAdmissionLimit",
    "globalBlockingAdmissionLimit",
    "globalProcessAdmissionLimit",
    "activeSessionLimit",
  ]) {
    assert.ok(restartPredicate.includes(`draft.${field}`));
  }

  for (const hotField of [
    "toolProfile",
    "permissionMode",
    "allowedCommands",
    "workspaceLocalEntries",
    "workspaceScriptExtensions",
  ]) {
    assert.ok(!restartPredicate.includes(`draft.${hotField}`));
  }

  assert.match(
    savePolicy,
    /if \(requiresRestart\) \{\s*await promptServiceRestart\(mcpStatus[\s\S]*await promptServiceRestart\(actionsStatus/,
  );
});
