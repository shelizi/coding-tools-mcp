import { cp, mkdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawn } from "node:child_process";
import { extractInlineAssets } from "../../../scripts/extract-sveltekit-inline.mjs";

const nodeAgentRoot = path.dirname(fileURLToPath(new URL("../package.json", import.meta.url)));
const repoRoot = path.resolve(nodeAgentRoot, "../..");
const outDir = path.join(nodeAgentRoot, "dist", "ui");
const staticDir = path.join(nodeAgentRoot, "management-static");

function run(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: repoRoot,
      env,
      stdio: "inherit",
      shell: process.platform === "win32",
    });
    child.on("error", reject);
    child.on("exit", (code) => {
      if (code === 0) resolve();
      else reject(new Error(`${command} ${args.join(" ")} exited ${code}`));
    });
  });
}

const env = {
  ...process.env,
  CTMCP_UI_HOST: "node",
  CTMCP_UI_BASE: "/ui",
  CTMCP_UI_OUT: outDir,
};

await mkdir(outDir, { recursive: true });
await run("pnpm", ["exec", "vite", "build"], env);
await extractInlineAssets(outDir, "/ui");
try {
  await cp(staticDir, outDir, { recursive: true, force: true });
} catch (error) {
  if (error && error.code !== "ENOENT") throw error;
}
console.log(`Node management UI written to ${outDir}`);
