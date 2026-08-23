# Versioning

`package.json` is the only release version that maintainers edit directly.

For a new patch release, run:

```powershell
pnpm run version:patch
```

This updates `package.json` once and synchronizes the version required by pnpm and Cargo. Tauri reads the application version directly from `package.json` through `src-tauri/tauri.conf.json`.

Portable retries must not run the patch command again. Build or retry the current version with:

```powershell
pnpm run desktop:portable
```

The Rust portable ZIP uses the short Windows-safe name `ctmcp-<version>-win64.zip`. Its expanded latest-build directory is always `dist-portable/ctmcp-win64/` and is replaced on each successful build. The executable inside the portable package is `ctmcp.exe`.

CI runs `pnpm run version:check` to reject committed version drift.
