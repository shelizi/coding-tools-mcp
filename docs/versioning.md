# Versioning

`package.json` is the only release version that maintainers edit directly.

For a new patch release, run:

```powershell
npm run version:patch
```

This updates `package.json` once and synchronizes the version required by npm and Cargo. Tauri reads the application version directly from `package.json` through `src-tauri/tauri.conf.json`.

Portable retries must not run the patch command again. Build or retry the current version with:

```powershell
npm run desktop:portable
```

The Rust portable ZIP remains versioned as `Coding.Tools.MCP_<version>_x64_portable.zip`. Its expanded latest-build directory is always `dist-portable/Coding.Tools.MCP_x64_portable/` and is replaced on each successful build.

CI runs `npm run version:check` to reject committed version drift.
