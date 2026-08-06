# Coding Tools MCP Node Agent

Pure Node.js/TypeScript implementation of Coding Tools MCP. It does not bundle an application EXE, native Node addon, Rust sidecar, FRP, or Cloudflare Tunnel.

## Implemented

- MCP Streamable HTTP endpoint with protocol versions `2025-11-25`, `2025-06-18`, and `2025-03-26`
- Rust-compatible MCP response contracts with bounded UTF-8 summaries, one structured payload copy, single-image responses, stable error envelopes, filesystem error codes, and redaction before serialization
- Per-runtime OAuth Authorization Code flow with PKCE S256, scoped well-known metadata, and Rust-compatible single-use authorization codes
- Rust-generated `advanced`, `read-only`, `compat-readonly-all`, `guarded-core`, and `trusted-core` catalogs with profile-specific schemas, annotations, tool names, and revisions
- Multiple configured workspace folders with Rust-compatible per-conversation selection, bounded 128-context LRU, per-folder cwd restoration, canonical duplicate-root rejection, stable profile/history metadata, and per-runtime legacy fallback isolation
- Folder-scoped process sessions, operation fingerprints, pending permissions, edit proposals, admission, and resource locks, with immutable resume routing to the original workspace
- Guarded permission requests and `request_permissions` resume
- BOM-aware UTF-8/UTF-16 read, search, project inspection, encoding-preserving edits, bounded edit proposals, structured patch recovery, file operations, and content-identified bounded image responses
- Canonical workspace containment across file, patch, formatter, Git, cwd, and process workdir inputs, with absolute-path denial, parent traversal rejection, external symlink blocking, and Rust-compatible protected write paths
- Gitignore-aware bounded workspace traversal shared by list, search, project inspection, and formatter project scopes
- Rust-compatible structured Git status/diff/log/show/blame contracts plus guarded branch/stage/commit/restore mutations
- Path-scoped binary snapshot protection for `git_restore`, allowing index/worktree rollback when a restore operation fails
- Rust-compatible retained command lifecycle with operation reattachment, output cursors, interactive stdin, detached cleanup, process-tree termination, post-check verification, and `exec_many` auto/DAG scheduling
- Pure-JavaScript Windows process-startup control with Rust-compatible launch slots, spacing, early probing, deterministic `0xC0000142` retries, circuit breaking, cancellation, bounded diagnostics, and buffered-output preservation
- Rust-compatible WSL UNC workspaces with distribution-scoped path containment and shell-free `wsl.exe` routing for commands, post-checks, formatter mirrors, and custom adapters
- Rust-compatible persistent redacted tool-usage JSONL analytics with rotation, scopes, percentiles, async child lifetimes, burst/orchestration analysis, and parallelism evidence
- Rust-compatible central sensitive-output redaction with protected credential-path detection and persistent process-session withholding
- Rust-compatible pre-dispatch command and mutation policy with allowlists, workspace-local executable checks, explicit shell confirmation, protected environment variables, and bounded payloads
- Durable Rust-compatible history/task/operation state
- Rust-compatible Harness baselines with canonical workspace IDs, per-file SHA-256 fingerprints, stale-task gates, automatic expected-state refresh, task evidence, and per-workspace operation JSONL
- Rust-compatible shared history archive with atomic index/Markdown writes, cross-runtime directory locking, scan/rebuild validation, bounded inherited summaries, and idempotent redacted checkpoints
- Blocking/process admission limits and workspace mutation locks
- Built-in WSS tunnel protocol v3 with Ed25519 device authentication, dynamic WorkerPolicy scaling, and Rust-compatible cancellation of in-flight local responses
- AES-256-GCM encrypted tunnel identity file
- Versioned public configuration schema with automatic migration from legacy plaintext settings
- AES-256-GCM encrypted Agent secret store for OAuth password, client secret, token secret, and tunnel enrollment URL
- Rust-compatible read/search/project contracts and safe `format_files` plan/check/apply flow
- Optional loopback-only React management UI for status, workspaces, limits, OAuth, and tunnel settings
- Bootstrap/React-Bootstrap responsive layout, TanStack Query polling/cache, light/dark/system themes, and structured workspace editing
- Installable PWA standalone window using the same locally bundled browser UI, without shipping a custom desktop EXE or loading a CDN
- Atomic JSON configuration writes; runtime settings are applied only after an explicit Agent restart
- Management API protected by loopback/Host checks, same-origin validation, CSP, and a per-start random token
- Built-in WSS forwarding restricted to scoped MCP and OAuth routes; `/ui` and `/admin` are never tunnel routes
- Local health and activity Dashboard with bounded tool telemetry, admission load, command-session state, tasks, and tunnel worker metrics
- Dashboard session summaries deliberately omit commands, arguments, environment variables, stdin, retained output, post-check details, fingerprints, and operation summaries

## Requirements

- Node.js 22 or later
- Git for Git and patch tools
- Windows with WSL installed when using `\\wsl.localhost`, `\\wsl$`, or extended WSL UNC workspaces
- The command-line programs invoked by the MCP tools must already be installed

The production dependencies are JavaScript-only: `ws` for WebSocket transport plus `pngjs` and `jpeg-js` for bounded image decoding and encoding. React, React-Bootstrap, Bootstrap, TanStack Query, and Webpack are development/build dependencies whose browser output is bundled into `dist/ui`. CI/package checks use an explicit production-dependency allowlist and reject `.exe`, `.dll`, `.node`, and package lifecycle install scripts.

## Windows portable release

Build both Windows x64 portable editions from one verified build:

```powershell
npm run portable
```

- `bundled-node` includes `runtime/node.exe` and needs no system Node.js after extraction.
- `system-node` omits Node.js and requires Windows x64 Node.js 22 or later as `node.exe` on `PATH`.

Build a single edition with `npm run portable:bundled` or `npm run portable:system`. Neither edition requires npm after extraction.

The Node Agent application version comes from `package.json`; the portable wrapper version comes from `portable-version.json`. They are intentionally independent. See `../../docs/node-agent-portable.md` for filenames, archive contracts, release checks, and Skill versioning policy.

Both launchers default to `%LOCALAPPDATA%\CodingToolsMCPNode`, so extracted upgrades and edition switches reuse the current user's settings and encrypted state. Define `CTMCP_DATA_DIR` before launch to override the location or to use the package-local `data` directory.

## Local run

```powershell
$env:CTMCP_WORKSPACES = "E:\repo-a;E:\repo-b"
$env:CTMCP_OAUTH_CLIENT_ID = "chatgpt"
$env:CTMCP_OAUTH_PASSWORD = "replace-this"
$env:CTMCP_OAUTH_TOKEN_SECRET = "replace-with-a-strong-random-secret"

npm install
npm run build
npm test
npm start
```

For UI development, keep the normal Agent running from the last successful build and rebuild the React assets on source changes:

```powershell
npm run dev:ui
```

A normal `npm run build` compiles both the Node server and the production React bundle.

`CTMCP_WORKSPACES` uses the platform path delimiter: `;` on Windows and `:` on Unix.

## WSL workspaces

On Windows, workspace folders may use `\\wsl.localhost\<distribution>\...`, `\\wsl$\<distribution>\...`, or the extended `\\?\UNC\wsl.localhost\...` form. The Agent canonicalizes these paths without changing Linux path case. Distribution names are compared case-insensitively, while the Linux path remains case-sensitive.

Selecting a WSL workspace validates the distribution and directory with a shell-free `wsl.exe --distribution <distribution> --cd <linux-path> --exec test -d .` invocation. Commands, post-checks, formatter mirrors, built-in adapters, and workspace custom adapters then run through the same argument-array wrapper:

```text
wsl.exe --distribution <distribution> --cd <linux-cwd> --exec <program> <args...>
```

The wrapper does not interpolate a host shell. Same-distribution UNC arguments are converted to Linux paths. Cross-distribution UNC paths and Windows drive paths such as `C:\...` are rejected before process creation; use a workspace-relative path or the Linux mount form such as `/mnt/c/...`. WSL workspaces accept `shell=sh`, which runs as `sh -c`; `shell=cmd` and `shell=powershell` are unavailable because they would execute outside the selected distribution.

The normal test suite uses injected WSL runners and does not require WSL. To run the opt-in live routing test on a WSL-enabled Windows host:

```powershell
$env:CTMCP_TEST_WSL_DISTRO = "Ubuntu-24.04"
$env:CTMCP_TEST_WSL_PATH = "/tmp"
node --test test/wsl.test.mjs
```

Default local endpoint:

```text
http://127.0.0.1:3789/mcp
```

Browser management UI:

```text
http://127.0.0.1:3789/ui
```

The UI is optional and is never opened automatically. MCP, OAuth, command sessions, and Built-in WSS continue to run without a graphical environment. Disable the UI explicitly with:

```powershell
npm start -- --no-ui
```

When supported by the browser, the UI can be installed as a PWA and opened in a standalone application window. The Service Worker does not cache the management page, configuration, runtime token, or Dashboard responses. JavaScript, CSS, icons, and the manifest are served from the Agent itself; the UI does not depend on a CDN or external web asset.

The React UI includes a live Dashboard for runtime health, RSS memory, blocking/process admission load, pending permissions, command-session state, tool latency/error statistics, recent durable activity, task counts, and Built-in WSS worker metrics. TanStack Query refreshes status and telemetry every five seconds, while configuration is refreshed only on initial load, explicit refresh, or save so background polling does not overwrite an in-progress form.

## Built-in WSS

The public URL must be issued by the existing Coding Tools built-in tunnel server:

```powershell
$env:CTMCP_BUILTIN_PUBLIC_URL = "https://tunnel.example/builtin/clients/my-client/mcp"
$env:CTMCP_BUILTIN_ENROLLMENT_URL = "https://tunnel.example/_tunnel/enroll/ONE_TIME_CODE"
```

The first start generates an Ed25519 device key, performs one-time enrollment, and stores the private identity encrypted with `CTMCP_OAUTH_TOKEN_SECRET`. Later starts no longer require the enrollment URL.

The agent starts one bootstrap connection. After authentication, the tunnel server's v3 `WorkerPolicy` controls startup capacity, minimum idle workers, demand-driven scale-up, staged scale-down, connection limits, burst warming, and worker recycling.

While a worker is waiting for local HTTP response headers or streaming the response body, it continues consuming the Rust-supported live frames. A matching `cancel` aborts the local fetch, cancels an active response reader, emits neither `response_end` nor an error, and returns the same worker to `ready`. WebSocket ping/pong heartbeat handling remains active throughout local I/O. Abortable queue waits remove the losing waiter when local I/O completes first, so a stale waiter cannot consume the next request or control frame.

### Production WSS E2E

The normal test suite runs complete local protocol integration tests, including enrollment, Ed25519 challenge authentication, streaming forwarding, cancellation before local headers and during response streaming, heartbeat liveness, worker reuse, WorkerPolicy scale-up/down, and encrypted identity persistence. A separate opt-in runner verifies the same Agent against an actual production Built-in WSS server and then performs public OAuth PKCE and MCP calls through that tunnel:

```powershell
$env:CTMCP_E2E_BUILTIN_PUBLIC_URL = "https://tunnel.example/builtin/clients/my-client/mcp"
$env:CTMCP_E2E_BUILTIN_ENROLLMENT_URL = "https://tunnel.example/_tunnel/enroll/ONE_TIME_CODE"
$env:CTMCP_E2E_OAUTH_PASSWORD = "replace-this"
npm run test:wss:production
```

The runner verifies public OAuth metadata, authorization-code PKCE, token exchange, MCP initialization, all 50 tool declarations, workspace selection, and `read_file` marker round-trip. It is intentionally excluded from `npm test` because enrollment codes are one-time production credentials. For repeat runs, set `CTMCP_E2E_DATA_DIR` and the same `CTMCP_E2E_OAUTH_TOKEN_SECRET`; the encrypted production test identity will be reused and a new enrollment URL is not required.

## Configuration

| Variable | Default | Purpose |
|---|---:|---|
| `CTMCP_HOST` | `127.0.0.1` | Local bind host |
| `CTMCP_PORT` | `3789` | Local HTTP port |
| `CTMCP_WORKSPACES` | current directory | Allowed workspace folders |
| `CTMCP_DATA_DIR` | OS user data directory | Durable state, encrypted Agent secrets, and tunnel identity; keep custom Windows locations user-private |
| `CTMCP_PERMISSION_MODE` | `trusted` | `read-only`, `guarded`, `trusted`, or `dangerous` |
| `CTMCP_TOOL_PROFILE` | `core` | `core`, `trusted-core`, `guarded-core`, `read-only`, `advanced`, or `compat-readonly-all` |
| `CTMCP_UI_ENABLED` | `true` | Enables the loopback-only browser/PWA management UI |
| `CTMCP_CONFIG_FILE` | `<data-dir>/agent.json` | Persistent configuration file path |
| `CTMCP_BLOCKING_CONCURRENCY` | `32` | File/Git blocking tool limit |
| `CTMCP_PROCESS_CONCURRENCY` | `16` | Process tool limit |
| `CTMCP_ACTIVE_SESSION_LIMIT` | `128` | Retained active process limit |
| `CTMCP_MAX_OUTPUT_BYTES` | `1048576` | Retained output per stream |
| `CTMCP_ALLOWED_COMMANDS` | Rust default allowlist | Comma-separated command additions; defaults remain enabled |
| `CTMCP_WORKSPACE_LOCAL_ENTRIES` | `true` | Allows configured executable entries that resolve inside the selected workspace |
| `CTMCP_WORKSPACE_SCRIPT_EXTENSIONS` | `.exe,.bat,.cmd,.ps1` | Comma-separated workspace-local executable extensions |
| `CTMCP_MAX_PATCH_BYTES` | `200000` | Base mutation payload limit; batch tools use the Rust multiplier |
| `CTMCP_PUBLIC_BASE_URL` | derived | External OAuth/MCP route base |
| `CTMCP_BUILTIN_PUBLIC_URL` | unset | Enables built-in WSS |
| `CTMCP_BUILTIN_ENROLLMENT_URL` | unset | First-run device enrollment URL |

Without `--config`, the Agent reads and writes `<data-dir>/agent.json`. A different JSON file can be supplied with:

```powershell
npm start -- --config .\node-agent.json
```

Environment variables take precedence over the JSON file and encrypted secret store. The UI edits the saved JSON values, reports the effective runtime state separately, and lists active environment overrides so an override is not accidentally persisted into the file. Password, client secret, enrollment URL, and token secret values are never returned by the management API; blank secret fields preserve the existing encrypted value.

### Configuration schema and secret files

`agent.json` uses `schema_version: 1` and contains only non-sensitive settings. Runtime secrets are stored separately under the effective data directory:

```text
agent.json                    public, versioned settings
agent-secrets.enc.json        AES-256-GCM encrypted secrets
agent-secrets.key             random 256-bit local master key
builtin-tunnel-identity.enc.json  encrypted tunnel device identity
```

A minimal configuration looks like:

```json
{
  "schema_version": 1,
  "host": "127.0.0.1",
  "port": 3789,
  "toolProfile": "core",
  "oauth": { "clientId": "chatgpt" },
  "policy": {
    "allowedCommands": ["company-tool"],
    "workspaceLocalEntries": true,
    "workspaceScriptExtensions": [".exe", ".bat", ".cmd", ".ps1"],
    "maxPatchBytes": 200000
  },
  "folders": [
    { "id": "repo", "name": "Repo", "path": "E:\\repo" }
  ]
}
```

On first startup with a legacy configuration, the Agent writes the secrets to `agent-secrets.enc.json`, removes plaintext secret fields from `agent.json`, adds `schema_version: 1`, and deletes the legacy plaintext `oauth-token-secret` file after successful migration. Future schema versions are rejected instead of being interpreted incorrectly.

The encrypted file and master key are both local files. They prevent accidental disclosure through configuration sharing, logs, backups of `agent.json`, and management API responses, but they are not a substitute for an OS keychain against an attacker running as the same user. Files use mode `0600` on POSIX; Windows inherits the ACL of `CTMCP_DATA_DIR`.

Secret updates are written to the effective data directory selected by `CTMCP_DATA_DIR` or `dataDir`. Without an environment override, changing `dataDir` through the management UI creates a complete encrypted secret store in the new directory before updating `agent.json`. The old encrypted store is not deleted automatically because another configuration may share that data directory. If the environment override is later removed, that intentionally selects a different state and secret directory.

## Tool profiles

The persisted default is `toolProfile: "core"`. It resolves to `trusted-core` when `permissionMode` is `trusted` or `dangerous`, and automatically resolves to `guarded-core` for safer permission modes. `CTMCP_TOOL_PROFILE` overrides the saved value for the current process. The management UI shows the configured and effective profiles separately because profile changes require an Agent restart.

The catalogs are generated directly from the Rust registry:

- `trusted-core`: 35 core tools.
- `guarded-core`: the trusted core plus `request_permissions`.
- `read-only`: 18 diagnostic, read, retained-session, Git inspection, and image tools.
- `advanced`: the complete 50-tool contract.
- `compat-readonly-all`: all 50 tools with Rust-compatible read-only, non-destructive, idempotent, closed-world annotations.

`tools/list`, `server_info`, and `/mcp/info` expose the same effective profile, tool set, and profile-specific revision. `/health` and the management status API report that profile, revision, and matching tool count. A tool hidden by the profile is rejected with `UNKNOWN_TOOL` before policy checks, admission queues, workspace locks, or filesystem/process side effects, including calls made directly through the internal `callTool` API.

## Command and mutation policy

Every `exec_command`, every child of `exec_many`, and every post-check is validated before process creation. Exactly one of `program`, `cmd`, or `script` is accepted. Shell-free `cmd` is parsed into a program and argument vector; chaining, redirection, and expansion require an explicit shell mode and `confirm=true`.

The Rust default command allowlist is always retained, while `policy.allowedCommands` and `CTMCP_ALLOWED_COMMANDS` add project-specific commands. Executables containing a path must exist and resolve inside the selected workspace, with an allowed workspace script extension. Safe permission modes block network-looking commands; dangerous commands require confirmation; `.git` and `.github` remain protected. Process environment overrides cannot replace `PATH`, `PATHEXT`, `COMSPEC`, loader injection variables, or their macOS equivalents.

Patch, edit, file-operation, and formatting payloads are rejected before locks or filesystem writes when they exceed the configured Rust-compatible bounds. `exec_many` validates all children before starting the first process, so a later rejected command cannot leave partial execution behind.

## Harness baseline and operation evidence

`start_task` captures the same Rust baseline model: canonical-path workspace ID, Git branch and HEAD, a sorted per-file entry list, SHA-256, byte count, binary detection, and a deterministic worktree fingerprint. The capture skips `.git`, `.mcp-probe-kit`, `node_modules`, `target`, `dist`, `build`, and `.svelte-kit`, and does not follow symlinks. Existing Node tasks stored under a configured folder ID are migrated to the canonical Rust workspace ID when that workspace is first used.

For an active task, `exec_command`, real patch/edit/file operations, formatter apply, and real Git branch/stage/commit/restore operations verify the task baseline before starting. A changed branch or HEAD returns `BASELINE_STALE`; an unexpected worktree fingerprint returns `FILE_CHANGED_EXTERNALLY`. Dry runs remain available. Successful tracked operations refresh the expected fingerprint after task evidence is persisted. `exec_many` remains outside this gate because the Rust dispatcher does not classify it as a baseline-enforced operation.

`harness_status` calculates `baseline_matches`, capabilities, stable reasons, and recovery actions. `project_state` and `change_summary` compare the task baseline with current files and return added, modified, deleted, or unchanged states with current hashes and byte counts. Task events remain durable evidence. Rust currently returns empty `verification` and `risks` arrays and reports `rollback_capability: "not_available_in_foundation"`; Node preserves that exact foundation contract rather than adding unsupported verification or rollback behavior.

Tracked edit/exec/Git operations append Rust-shaped `started` and `completed` or `failed` records to `<dataDir>/harness/workspaces/<workspace-id>/operations.jsonl`. Logs are isolated by canonical workspace ID, survive restart, use offset pagination, and ignore a malformed or partial writer tail instead of exposing an incomplete record.

## Retained process session lifecycle

`exec_command` retains a bounded process session when a command outlives the initial response. Ordinary commands may have no operation ID; an explicit `operation_id` provides idempotent reattachment, while safe automatic deduplication uses `auto:<fingerprint>` and only reuses a completed session for 30 seconds. Conflicting explicit operation IDs return structured fingerprint and session details. Operation and resource-lock wait times are included in responses.

`wait_command`, `resolve_operation`, `list_sessions`, `send_input`, `kill_session`, and `read_output` use the Rust lifecycle fields: interactive and stdin-open state, first-output and elapsed timing, termination reason, recoverability, suggestions, heartbeat timing, post-check state, output references, byte offsets, UTF-8 alignment, cursor expiry, and bounded delta pagination. Output cursors advance only for stdout or stderr events; exit, finalization, and stdin state changes do not create phantom output pages.

Retained output events are bounded to 1 MiB. Finalized sessions remain available for up to 15 minutes, with at most 128 finalized summaries retained. An interrupted MCP request starts a 90-second detached grace period; reattachment cancels cleanup, while an unclaimed session is terminated with `detached_timeout`. Agent shutdown finalizes running sessions with the recoverable `server_restart` reason. `tty=true` follows the Rust pipe-backed interactive contract and keeps stdin open without adding a native PTY dependency.

## Persistent tool usage analytics

Each top-level MCP tool request is appended as a centrally redacted schema-v7 JSONL record under `<dataDir>/logs/mcp-tool-usage.jsonl`. The non-blocking writer queue is bounded to 1,024 pending records; overload is dropped and reported on the next accepted record through `telemetry_dropped_before`. The active file rotates at 20 MiB and keeps five prior files. Queries only consume newline-terminated records, so an in-progress or interrupted writer tail is ignored rather than parsed as corrupt telemetry. Internal `request_permissions` resume calls are not counted as additional client requests.

`query_tool_usage` supports Rust-compatible `current_runtime`, `current_version`, and `all` scopes plus tool, outcome, error-only, timestamp, and minimum-duration filters. Aggregates include calls, errors, warnings, queue and lock waits, request/response bytes, average, p50, p95, maximum duration, slowest responses, largest responses, formatting metrics, repeated identical errors, and retained-session coordination signals.

Process finalization writes a separate `async_session_finalized` event with child lifetime, first-output timing, termination reason, exit code, and stream byte counts. Performance analysis separates server execution from observed client-orchestration gaps, groups activity bursts, identifies sequential `exec_command` batching opportunities, and summarizes `exec_many` parallelism observations with deterministic statistical confidence. Payload records are opt-in and remain redacted; compact query results omit arguments and output bodies by default.

The management Dashboard keeps its bounded in-memory recent activity view and adds a cached persistent current-version summary, allowing aggregate usage, async lifetime, burst, and parallelism data to remain available after an Agent restart.

## Shared history archive

`history_session_bootstrap`, `history_session_checkpoint`, and `history_session_validate` now use the same `docs/history-session/index.json` version-1 format and numbered Markdown documents as Rust. The former Node-only `node-agent-index.json` format is no longer created. Missing or corrupt indexes are rebuilt from Markdown metadata, while duplicate session keys and sequence gaps remain explicit conflicts instead of being silently reassigned.

Rust and Node coordinate through the same atomic `.history.lock.d/owner.json` protocol. Lock acquisition retries every 10 ms for up to five seconds, records `history_lock_wait_ms`, and can recover an abandoned lock directory after 30 seconds. Owner tokens prevent an older holder from removing a replacement lock. Markdown and index updates use write-sync-rename transactions, so readers never observe a partially serialized checkpoint or index.

Bootstrap returns at most 12 detailed prior summaries and 256 history numbers, with omission counts, a SHA-256 content digest, bounded latest handoff, and a non-recursive inherited summary. Checkpoints preserve the bootstrap `session_key` and `current_path`, redact sensitive content, generate deterministic turn IDs when omitted, replace changed turns in place, and ignore exact retries without duplicating Markdown blocks. Validation reports missing numbers, duplicate mappings, invalid names, empty files, and index status; repair only rebuilds the derived index and never fabricates or deletes history documents.

## Edit proposals and patch recovery

When a guarded exact `edit_file` replacement does not match but one whitespace-flexible candidate is unambiguous, the Agent returns `status: "proposal_required"` without writing. Proposals are scoped to the current Agent runtime, expire after five minutes, and are bounded to 200 retained entries. The response includes the exact candidate, proposed content hash, accepted formats, size limits, and the preferred follow-up format.

A proposal can be applied unchanged (`accept`), with a complete replacement, or with a restricted single-file, single-hunk unified diff. Patch and replacement inputs are mutually exclusive. Proposal application rechecks both the complete file SHA-256 and the exact candidate slice; changed files, changed candidates, missing IDs, and expired IDs return structured conflict errors. Dry runs preserve the proposal, while a successful real write consumes it.

`patch_check` and `apply_patch` perform a TypeScript preflight before invoking Git. Missing or ambiguous hunk contexts include nearby lines and concrete recovery actions. Multiple failed hunks are returned together as `PATCH_PREFLIGHT_FAILED`, allowing callers to repair the complete patch in one iteration or switch to precise `edit_file` operations. Expected file hashes are also checked before Git and return guarded read/rebuild actions on conflict.

## Text encoding safety

A shared strict decoder handles UTF-8 with or without a BOM and BOM-marked UTF-16LE or UTF-16BE. `read_file`, `read_many`, `search_text`, and project manifest inspection use the same decoder. Invalid byte sequences return `UNSUPPORTED_ENCODING`; BOM-less data containing NUL bytes near the beginning returns `BINARY_FILE`. File-size limits are checked before decoding text.

`edit_file` and `edit_many` hash the original bytes, edit decoded text, then encode the result using the original encoding and BOM. Rollback also restores the original raw bytes, so UTF-16 byte order and UTF-8 BOMs are not silently converted. Git patch operations remain UTF-8-only and reject UTF-16 during preflight because Git applies patches against raw byte-oriented file content.

## Ignore-aware workspace traversal

`list_files`, `search_text`, `project_map`, and directory/project formatter scopes share one bounded walker. It evaluates root and nested `.gitignore` and `.ignore` files, plus repository-local `.git/info/exclude` when available. Git-style root anchoring, globstar patterns, character classes, directory rules, and negation are supported; a child can only be re-included after its ignored parent directory is also re-included.

`include_ignored=true` bypasses Gitignore rules and Rust-compatible default exclusions such as `node_modules`, `target`, `dist`, build caches, and virtual environments. It never exposes `.git` internals. `include_hidden` remains an independent switch, including for an explicitly selected hidden start directory.

Traversal uses `lstat`, reports symlinks without following them, rejects starts outside the configured workspace, and bounds depth, returned entries, and total visited directory entries.

## Structured Git read contracts

`git_status` reports whether the selected path is inside a repository, the current branch and full HEAD, upstream tracking, ahead/behind counts, clean state, bounded porcelain entries, and rename source paths. Non-repository paths return an empty successful result with a diagnostic warning rather than a generic Git failure.

`git_diff` can combine unstaged and staged changes in one response, applies workspace-safe path filters, clamps context to the Rust maximum of 20 lines, and returns normalized-argument metadata. Responses include the bounded unified diff, structured changed-file summaries, true byte truncation, and stable warnings. Setting both `staged=false` and `unstaged=false` returns an empty diff.

`git_log` performs one-record look-ahead so `truncated` is accurate, and returns full and short hashes, author identity/date, normalized ref/path metadata, and limit warnings. `git_show` uses the same bounded context and path rules and returns both content and structured files. `git_blame` parses line porcelain into bounded records containing commit, original/current line numbers, author metadata, summary, and source content. All Git subprocesses disable interactive credential prompts.

## Bounded image handling

`view_image` identifies PNG, JPEG, GIF, and WebP from file content rather than the filename extension. Responses include the effective MIME type, width, height, output bytes, original metadata, resize state, base64, data URL, warnings, and MCP image content when requested. The source file is never modified.

PNG and JPEG inputs are fully decoded with bounded pixel and memory limits. When dimensions exceed `max_width` or `max_height`, they are resized proportionally with an alpha-aware bilinear filter. PNG output remains PNG when it fits `max_bytes`; otherwise PNG and JPEG images are encoded as JPEG using the Rust-compatible quality sequence 85, 70, 55, and 40. If no result fits, the tool returns `OUTPUT_TOO_LARGE`.

GIF and WebP containers are structurally validated and their dimensions are reported. The pure-JavaScript runtime does not re-encode those formats; when they require resizing, the tool preserves the original valid image and returns a stable warning, or returns `OUTPUT_TOO_LARGE` when the unchanged bytes exceed the requested limit. Invalid or truncated images return `BINARY_FILE`.

## Formatter and file transaction safety

`format_files` supports built-in adapters plus workspace-defined adapters from `.coding-tools/formatters.json`:

```json
{
  "formatters": {
    "company-template": {
      "program": "tools/company-formatter.cjs",
      "extensions": ["tmpl"],
      "args": ["--write", "{files}"]
    }
  }
}
```

Custom `program` and optional `config` paths must be relative workspace paths, may not traverse symbolic links, and cannot reuse a built-in adapter ID. Supported placeholders are `{files}`, `{file}`, `{workspace}`, and `{config}`. Custom formatter execution requires `confirm=true` after reviewing `mode=plan`.

Every formatter runs in an isolated `.coding-tools-format/<id>` mirror. The real workspace is never used as formatter cwd. The Agent snapshots the mirror after each formatter group and rejects any added, deleted, or changed file outside that group's selected paths with `FORMAT_UNEXPECTED_CHANGES`. Only validated UTF-8 outputs are considered. `mode=check` discards the mirror; `mode=apply` rechecks every original SHA-256 before writing and rolls back already-written files if a later write fails.

`file_ops` similarly performs complete preflight before mutation, rejects protected paths and symlink traversal, stages file contents through temporary files, rechecks source/destination versions, and restores all backups if file replacement or a later directory operation fails. Git mutation tools support expected-HEAD guards and dry-run; `git_commit` requires a clean index by default when it stages paths and restores that index if commit hooks or Git reject the commit.

## Management UI architecture

The UI build and runtime boundaries are intentionally separate:

```text
src/management.ts          loopback, Host, token, same-origin checks; management JSON APIs
src/managementUi.ts        minimal HTML shell and allowlisted static asset adapter
ui/src/                    React application, components, hooks, API client, and styles
ui/public/                 PWA manifest, icon, and non-caching Service Worker
dist/ui/                   production Webpack output shipped with the package
```

`management.ts` contains no page markup, CSS, DOM code, React components, or form state. The HTML shell only carries the per-process management token in a no-store meta element and loads same-origin `app.js` and `app.css`. The compiled bundle is process-independent and does not contain that token.

React-Bootstrap components and Bootstrap's grid/utilities provide the responsive layout. TanStack Query owns API polling, cache invalidation, and save refreshes. The settings screen uses typed, structured React state rather than parsing a free-form workspace textarea. Dynamic server values are rendered as React text nodes; the UI does not use `dangerouslySetInnerHTML`.

## Development verification

```powershell
npm run verify
npm run verify:repo
# Validate the Rust/Node parity roadmap:
npm run check:parity-todos
# Requires explicit production credentials:
npm run test:wss:production
```

`verify` is the pure Node package verification used before packing. `verify:repo` additionally runs the Rust catalog exporter, verifies the Desktop Client compatibility marker, validates the Rust/Node parity roadmap, and fails if a generated contract or roadmap reference has drifted. The Node Agent package version is an independent release version; `codingTools.clientVersion` and `src/clientVersion.generated.ts` record the Desktop Client version whose shared contracts were synchronized. Root `npm run version:sync` updates these values whenever the Desktop Client version changes. `test:wss:production` is deliberately separate and fails immediately unless production URL, OAuth, and enrollment or persisted-identity settings are supplied.

## HTTP endpoints

- `POST /mcp`
- `GET /health`
- `GET /ui` (loopback only, optional PWA)
- `GET /admin/api/status` (loopback only; runtime token required)
- `GET /admin/api/dashboard` (loopback only; runtime token required; bounded and redacted telemetry)
- `GET|PUT /admin/api/config` (loopback only; runtime token required)
- `GET /.well-known/oauth-authorization-server`
- `GET /.well-known/oauth-protected-resource/mcp`
- `GET|POST /oauth/authorize`
- `POST /oauth/token`

Path-scoped variants such as `/builtin/clients/<client-id>/mcp` and their RFC well-known metadata paths are supported.

## Management security

The management surface accepts only requests whose TCP peer and `Host` header are loopback addresses. API calls also require a random token generated at process start and a same-origin request. The token is injected only into the no-store HTML shell, is read by the React API client from a meta element, is absent from the static JavaScript bundle, and changes after every restart.

Built-in WSS accepts only the configured scoped MCP, OAuth, and well-known metadata paths. It rejects management routes and other localhost paths before issuing an HTTP request. For a remote headless machine, access the UI through SSH port forwarding so the Agent still sees a loopback connection.

## Dashboard privacy and bounds

The Dashboard is designed for operational visibility rather than command inspection. Session records include only a random session ID, workspace name/ID, workspace-relative working directory, state, timestamps, PID, exit code, verification status, and output byte counts. It never returns the command line, arguments, command fingerprint, environment, removed environment names, stdin, stdout/stderr content, output references, post-check commands/output, pending permission arguments, task objectives, or persisted operation error summaries.

The API returns at most 50 session summaries, 50 durable activity records, and 100 recent usage records. Tool aggregates use the most recent 1,000 calls. Health is `degraded` when an enabled tunnel is reconnecting/in error, or when at least 10 recent calls have an error rate of 25% or higher; it is `busy` when admission queues are non-empty, otherwise `healthy`.

All dynamic Dashboard values are rendered as escaped React text nodes. The UI does not use `dangerouslySetInnerHTML`, so server data is not interpreted as HTML.

## Intentionally unsupported

- FRP
- Cloudflare Tunnel / `cloudflared`
- Automatic downloading or launching of third-party tunnel executables
- Native desktop shell, system tray, and auto-start integration (the PWA standalone window is supported)
- Native Windows Credential Manager/DPAPI bindings
- Native Windows Job Object bindings
- Native Windows process error-mode / crash-dialog suppression bindings

On Windows, process termination currently uses `taskkill /T` and reports `process_tree_contained=false` because it is not kernel-enforced Job Object containment; on Unix it uses a detached process group and reports process-group containment. The pure-JavaScript startup controller cannot call Win32 `SetErrorMode`, so `startup.error_dialog_suppressed` remains `false` even though launch gating, probing, retries, and circuit breaking are active. Tunnel identity and Agent secrets use AES-256-GCM encrypted local files instead of a native keychain. Secret files use `0600` on POSIX; Windows inherits the ACL of `CTMCP_DATA_DIR`, so custom locations must remain user-private.
