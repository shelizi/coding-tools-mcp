# Intentional Node Agent divergences

These Rust/Desktop capabilities are outside the current pure Node Agent boundary and are not counted as active parity defects. Shared assertions listed in `manifest.json` still protect the overlapping MCP behavior.

- **FRP and Cloudflare process management (`ND-001`):** the Node package supports built-in WSS only and does not download or launch third-party tunnel executables.
- **Tauri desktop shell, tray, and native auto-start (`ND-002`):** the Node package is headless with an optional loopback PWA.
- **Native OS keychain or Windows DPAPI (`ND-003`):** secrets use AES-256-GCM files; Windows protection depends on the configured data-directory ACL.
- **Native Windows Job Objects (`ND-004`):** the package remains native-binary-free and uses `taskkill /T` on Windows.
- **Desktop-managed software installation (`ND-005`):** invoked tools must already be installed.
- **Actions and OpenAPI service (`ND-006`):** Node Agent remains MCP-only. It does not expose the Rust Actions listener, OpenAPI document, Actions authentication settings, or Actions tunnel routes. See `TODO-016-actions-openapi-decision.md`.
- **Static Bearer and no-auth MCP modes (`ND-007`):** Node Agent retains OAuth as its only MCP authentication mode. Rust static Bearer and loopback-only no-auth modes are intentionally not added.
- **Legacy JSON MCP transport (`ND-008`):** Node Agent supports streamable HTTP only. Rust `legacy-json` discovery and request behavior are intentionally not retained.
- **Desktop history-session activity viewer (`ND-009`):** Node Agent keeps its current dashboard, process-session, and tool-usage telemetry instead of reproducing the Rust per-history-session running, active, and inactive UI state.
- **Desktop runtime and port supervisor (`ND-010`):** Node Agent keeps duplicate-port validation and process-level restart, but not automatic free-port assignment, stale listener recovery, or separate MCP/Actions service supervisors.

The exclusions above must not suppress shared streamable-HTTP validation, response, security, cancellation, or telemetry checks. Those remaining gaps are tracked by `NP-025` through `NP-028`.
