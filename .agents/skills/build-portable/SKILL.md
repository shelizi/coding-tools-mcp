---
name: build-portable
description: Build and verify the Coding Tools MCP Windows portable release ZIP. Use when asked to compile, rebuild, package, or troubleshoot a portable desktop executable, especially when a packaged app opens localhost:1420 or reports ERR_CONNECTION_REFUSED.
---

# Build Portable

Run the deterministic project script from the repository root:

```powershell
pnpm run desktop:portable
```

The script builds the frontend, builds the Tauri release with the required `custom-protocol` feature, copies the executable, creates the versioned ZIP, and prints SHA-256 hashes.

Portable ZIP filenames and ZIP top-level directories are versioned. The expanded latest-build directory is stable and versionless (`dist-portable/Coding.Tools.MCP_x64_portable`) so each successful build replaces it instead of accumulating extracted copies.

## Guardrails

- `package.json` is the single source of truth for the release version. For every new requested portable release build, run `pnpm run version:patch` exactly once before compiling. This updates the source version and synchronizes the package/Cargo metadata; `src-tauri/tauri.conf.json` reads the version directly from `package.json`. A retry of the same failed build only reruns `pnpm run desktop:portable` and does not increment again.
- Never package an executable produced by bare `cargo build --release`. Without `custom-protocol`, Tauri treats the binary as development mode and navigates to `http://localhost:1420`.
- Never copy an older executable after a build timeout or failure. Keep the existing package unchanged and report the build error.
- If running Cargo manually for diagnosis, include `--features custom-protocol`.
- Treat the release compile-time error in `src-tauri/src/main.rs` as intentional; use the portable script instead of bypassing it.
- Build the ZIP from isolated staging. If the expanded portable executable is running and locked, accept the warning and deliver the newly generated ZIP; do not terminate the user's app implicitly.

## Verify

After the script succeeds:

- Confirm the stable expanded folder contains `Coding Tools MCP.exe`.
- Confirm the versioned ZIP contains its versioned top-level folder and executable.
- Report the ZIP path, byte size, and SHA-256 hash.
- Launch the packaged executable when desktop interaction is available and confirm it renders embedded UI rather than `localhost`.
