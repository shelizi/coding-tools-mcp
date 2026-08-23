import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";
import path from "node:path";

const host = process.env.TAURI_DEV_HOST;
const uiHost = process.env.CTMCP_UI_HOST === "node" ? "node" : "desktop";

export default defineConfig(async () => ({
  plugins: [
    {
      name: "ctmcp-host-adapter",
      enforce: "pre",
      resolveId(/** @type {string} */ id) {
        const normalized = id.replaceAll("\\", "/");
        if (
          id === "$lib/backend/host" ||
          normalized.endsWith("src/lib/backend/host") ||
          normalized.endsWith("src/lib/backend/host.ts")
        ) {
          return path.resolve(`src/lib/backend/host-${uiHost}.ts`);
        }
      },
    },
    tailwindcss(),
    sveltekit(),
  ],
  clearScreen: false,
  resolve: {
    alias: {
      "$lib/backend/host": path.resolve(`src/lib/backend/host-${uiHost}.ts`),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
