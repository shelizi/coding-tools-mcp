# Core / Platform Boundary

The Rust package supports two host modes:

- `desktop` (default): Tauri, system tray, webview, and IPC commands.
- headless core (`--no-default-features`): MCP, tools, workspaces, persistence,
  runtime supervision, tunnels, and OS primitives without Tauri or GUI libraries.

## Build contracts

```powershell
# Existing desktop behavior
cargo check --manifest-path src-tauri/Cargo.toml

# UI-independent core used by future Linux/macOS hosts
cargo check --manifest-path src-tauri/Cargo.toml --no-default-features --lib
cargo test --manifest-path src-tauri/Cargo.toml --no-default-features --test headless_core
```

The same headless contract is available as `npm run rust:check:headless` and
runs in a separate Linux CI job without installing Tauri system packages.

The desktop executable has `required-features = ["desktop"]`, so a headless
consumer never builds the Tauri entry point accidentally.

## Dependency rules

1. Code under `commands/` and the desktop `run()` entry point may use Tauri.
2. Core modules must not import Tauri. Async work goes through
   `task_runtime`, which delegates to Tauri for desktop builds and Tokio for
   headless builds.
3. Types intended for another host are exported through `core`; avoid exposing
   UI-specific handles or IPC types there.
4. New Linux/macOS hosts should depend on the headless core rather than copy
   business logic out of desktop commands.

Runtime start, stop, restart, status, port validation, and tunnel coordination
live in `application::runtime` and are re-exported through `core`. Tauri runtime
commands are deliberately thin adapters over these host-neutral services.

This boundary is intentionally host-oriented. A future CLI can be added as a
separate binary or crate without changing MCP or tool behavior.
