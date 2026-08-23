<!-- parity-id: NP-015 -->
<!-- parity-status: done -->
# NP-015 — WSL workspace execution and formatter routing

- Priority: P2
- Area: platform
- Status: done

## Gap

Resolved in Node Agent 0.23.0. Node now recognizes the Rust WSL UNC forms, preserves Linux path case, validates the selected distribution and directory, and routes command sessions, post-checks, formatter mirrors, and custom adapters through shell-free `wsl.exe` argument arrays.

## Rust evidence

- `src-tauri/src/workspace/location.rs`
- `src-tauri/src/platform/wsl.rs`
- `src-tauri/src/tools/exec.rs`
- `src-tauri/src/tools/file_action.rs`

## Node current state

- `packages/node-agent/src/wsl.ts`
- `packages/node-agent/src/config.ts`
- `packages/node-agent/src/workspace.ts`
- `packages/node-agent/src/policy.ts`
- `packages/node-agent/src/processes.ts`
- `packages/node-agent/src/gitProcess.ts`
- `packages/node-agent/src/gitTools.ts`
- `packages/node-agent/src/fileTools.ts`
- `packages/node-agent/src/formatterTools.ts`
- `packages/node-agent/src/taskTools.ts`
- `packages/node-agent/src/tools.ts`
- `packages/node-agent/test/wsl.test.mjs`

## Implementation summary

The pure-JavaScript WSL model parses `\\wsl.localhost`, `\\wsl$`, and extended UNC paths into a distribution plus normalized Linux path. Distribution matching is case-insensitive and Linux containment is case-sensitive. Workspace selection validates the directory using `wsl.exe --distribution <distro> --cd <path> --exec test -d .`.

Process launch uses `wsl.exe --distribution <distro> --cd <cwd> --exec ...` without host-shell interpolation. Explicit environment additions and removals are passed through Linux `env`; same-distribution UNC arguments become Linux paths; cross-distribution UNC and Windows drive arguments are rejected before session creation. WSL shell execution is limited to `sh -c`, matching Rust.

Git subprocesses used by Git tools, patch application, formatter scopes, and task baselines share one WSL-aware runner with `GIT_TERMINAL_PROMPT=0` set inside the distribution. Absolute Linux executables are normalized before policy checks: workspace-local entries retain the configured extension policy, while allowlisted system commands are accepted only from trusted system directories. Windows UNC host arguments and option-like environment names are rejected before `wsl.exe` launch.

Formatter discovery uses Linux `node_modules/.bin`, `.venv/bin`, and `venv/bin` candidates for WSL roots. Formatter mirrors and JavaScript custom adapters execute inside the selected distribution, using its `node` rather than the Windows Node executable. Formatter children run through a shell-safe `env -i` wrapper that retains only the required distro environment fields.

## Acceptance checklist

- [x] WSL UNC paths parse into distribution and Linux path.
- [x] Invalid, parent-traversing, host-drive, host-UNC, and cross-distribution paths are rejected.
- [x] Program, cwd, arguments, environment changes, and post-checks are translated without host-shell interpolation.
- [x] Absolute executables are normalized and limited to workspace-local entries or trusted system directories.
- [x] Git subprocesses route through WSL with non-interactive authentication.
- [x] Shell restrictions match Rust: `sh` only for WSL workspaces.
- [x] Formatter mirrors and custom adapters run in the selected distribution with a clean inherited environment.
- [x] Host workspace command and formatter behavior remains unchanged.

## Dependencies

Requires `NP-002` and `NP-009`; both are complete.

## Verification

Cross-platform fixtures cover all WSL UNC forms, case semantics, containment, exact `wsl.exe` argv, environment handling, workspace validation through an injected runner, command policy, and formatter routing. The live test is conditional on Windows plus `CTMCP_TEST_WSL_DISTRO`; it remains skipped unless explicitly enabled on a WSL host.
