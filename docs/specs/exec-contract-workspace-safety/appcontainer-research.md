# Windows AppContainer sandbox research

## Goal

Replace the current `policy_only` child-process boundary with a real Windows OS-level filesystem sandbox for `exec_command` without weakening the existing session, timeout, output, Job Object, or multi-folder routing contracts.

Do not report `sandbox_enforced=true` until the validation gates below pass for the real command process and its descendants.

## Confirmed platform path

Windows supports launching a desktop AppContainer by supplying `STARTUPINFOEXW` with a `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES` attribute that points to `SECURITY_CAPABILITIES`. `CreateAppContainerProfile` or `DeriveAppContainerSidFromAppContainerName` supplies the AppContainer package SID.

The AppContainer profile provides an AppContainer-owned `LOCALAPPDATA` area and redirects `TEMP`/`TMP` into that profile. Access to additional filesystem resources is granted through Windows ACLs to the package/capability SID.

The branch compile probe confirms the required windows-rs 0.61 APIs/features are present:

- `Win32_Security_Isolation`
- `Win32_Security_Authorization`
- `SECURITY_CAPABILITIES`
- `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
- `InitializeProcThreadAttributeList`
- `UpdateProcThreadAttribute`
- `CreateProcessW`

## Identity scope

Use a stable AppContainer profile/SID per selected workspace folder, not one global SID for the entire application.

Reason: a single SID that accumulates ACL grants across folders would allow a command routed to folder A to retain access to previously granted folder B. Per-folder identity preserves the one-call `workspace_folder_id` routing boundary.

Suggested moniker shape:

`CodingToolsMcp.Sandbox.<bounded-folder-fingerprint>`

The moniker must stay within the AppContainer profile naming limits and must not expose the raw workspace path.

## Proposed execution architecture

Keep the existing Rust runtime as the trusted coordinator. Introduce a small Windows sandbox broker/launcher that owns only process creation and stdio bridging:

1. Existing runtime resolves command, cwd, policy, routing, operation identity, locks, and timeout exactly as today.
2. Runtime launches the trusted broker under the existing process-start controller and Job Object containment.
3. Broker resolves/creates the per-folder AppContainer profile and obtains the package SID.
4. Broker applies the required filesystem ACL grants for the selected folder and read/execute grants required by the chosen runtime/toolchain.
5. Broker builds a `STARTUPINFOEXW` attribute list with `SECURITY_CAPABILITIES` and launches the real command with `CreateProcessW`.
6. Broker forwards stdin/stdout/stderr and mirrors the real command exit status.
7. Existing Rust session/timeout/kill/finalization code continues to track the broker process tree.

The broker must not own routing, policy, session retention, or command semantics.

## Major compatibility problem to prove

System executables and Windows runtime files are generally compatible with AppContainer access rules, but developer toolchains installed under user-controlled paths may not be. Rust/Cargo, Node/npm, Python, Git, Scoop-installed binaries, package caches, global configuration, and dynamically loaded dependencies can require read/execute access outside the selected workspace.

The sandbox must not solve this by granting broad write access outside the selected folder.

Preferred compatibility strategy:

- selected workspace folder: read/write/execute through its per-folder package SID;
- shared executable/toolchain roots: read/execute through a stable shared runtime capability SID rather than rewriting ACLs for every workspace package SID;
- temp/cache/home locations: redirect into AppContainer profile or a folder-local sandbox state directory where feasible;
- provision/cache runtime grants once and never recursively rewrite a large toolchain tree during a tool call;
- no persistent broad grant to the whole user profile;
- network capabilities remain explicit and are tested separately from filesystem isolation.

A trial per-workspace recursive RX grant was deliberately abandoned after `icacls` traversed roughly 71k Rust-toolchain files; later residue verification traversed 67,160 Rust files and 209,953 Node-root entries while finding no remaining Matrix SID. This is far too expensive for per-workspace or per-call setup and establishes a hard design constraint: runtime authorization must be stable/shared and pre-provisioned, while workspace write authorization remains folder-specific.

## Child-process inheritance risk

Do not assume that a command launched as AppContainer automatically keeps every descendant inside the same effective filesystem boundary. The production design must include an empirical child-process test and token inspection for a sandboxed process that spawns another process.

If descendants can escape the AppContainer identity, the broker design is insufficient and must be changed before integration.

## Validation gates

All of the following must pass before changing the public execution metadata from `policy_only` / `sandbox_enforced=false`:

1. The real command token is confirmed to contain the intended AppContainer package SID.
2. Writing and deleting inside the selected folder succeeds.
3. Writing, replacing, or deleting a sibling/outside folder is denied by the OS even when the command text is not recognized by policy heuristics.
4. A child/grandchild process created by the sandboxed command is subject to the same outside-write denial.
5. stdin, stdout, stderr, exit code, timeout, cancellation, and Job Object tree termination preserve current behavior.
6. `cmd`, PowerShell, Git, Python, Node, Cargo/Rust and representative package-manager flows have an explicit compatibility result.
7. Multi-folder routing proves folder A's sandbox identity cannot access folder B unless B was explicitly selected/granted for that execution.
8. Failure to create the profile, apply ACLs, or create the AppContainer process fails closed and reports a structured sandbox error; it must never silently fall back to unsandboxed execution while claiming enforcement.

## Integration stages

### R1 - API/compile probe

Status: passed.

The branch enables the windows-rs AppContainer/security feature modules and compiles `src-tauri/examples/appcontainer_api_probe.rs`.

### R2 - Runtime isolation probe

Status: passed.

`appcontainer_isolation_probe.rs` creates a disposable AppContainer profile and sibling `inside` / `outside` directories, grants the package SID read/execute access on the probe root and Modify access only on `inside`, then launches a copied probe executable with `SECURITY_CAPABILITIES`. Runtime result: child exit code 0, the inside write exists, and the outside write does not exist. This proves the filesystem denial is enforced by the AppContainer/DACL boundary rather than command-text policy.

### R3 - Descendant containment probe

Status: passed.

The sandboxed child verifies `TokenIsAppContainer`, then launches the same executable as a grandchild through ordinary `std::process::Command`; the grandchild independently verifies the same token flag. Runtime result: both inside writes exist, neither outside write exists, and the parent observes child exit code 0. This demonstrates descendant token/filesystem containment for ordinary child creation in the current prototype; it is not yet a claim about every specialized Windows process-creation API.

### R4 - Broker/stdin-stdout prototype

Status: passed.

`appcontainer_broker_probe.rs` launches the AppContainer command suspended, assigns it to a `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` Job Object, then resumes it. Inherited anonymous pipes preserve stdin/stdout/stderr and the real child exit code: the probe sends `broker-ping`, receives `stdout:broker-ping`, receives `stderr:appcontainer=true`, and observes exit code 23. A second 30-second AppContainer sleep child is terminated by closing its Job Object and is observed exited within five seconds. This validates the core broker/session bridge without moving routing or policy ownership into the broker.

### R5 - Toolchain compatibility matrix

Status: mapped; compatibility gaps remain and must be solved before production integration.

The final matrix is default-only and performs no runtime ACL mutation. Each case runs as a real AppContainer command under the R4 pipe/Job Object launcher:

| Tool | Result without runtime grant | Evidence |
| --- | --- | --- |
| `cmd.exe` | pass | exit 0, `cmd-ok` |
| Windows PowerShell | pass | exit 0, `powershell-ok` |
| PowerShell 7 (`pwsh`) | pass | exit 0, `pwsh-ok` |
| Git (`Program Files`) | pass | exit 0, version output |
| Node (Scoop) | pass | exit 0, `node-ok` |
| Python 3.12 (Scoop) | fail | exit `0xC0000135`, consistent with runtime/DLL load failure |
| npm (`npm.cmd`, Scoop) | fail | `cmd.exe` reports access denied when opening the user-installed script |
| rustc (rustup/Scoop) | fail | exit `0xC0000135` |
| Cargo check (rustup/Scoop) | partial | Cargo starts, exit 101 because it cannot execute `rustc -vV` |

The result is useful precisely because user-installed runtimes are not uniformly inaccessible: Node works without extra grants while Python/Rust/npm do not. Production therefore must discover the concrete executable/runtime dependency boundary rather than granting an entire user profile.

The scalable design is a stable private shared runtime capability SID derived from a fixed capability name. That capability is included in `SECURITY_CAPABILITIES` for every per-folder AppContainer while the corresponding SID receives read/execute ACLs on approved runtime roots once. Per-folder package SIDs remain the only principals granted mutable workspace access.

A subtle but important ACL rule was verified during this work: do not strip the ordinary inherited user/system ACLs from runtime or workspace directories merely to create the AppContainer boundary. Windows evaluates AppContainer access using the ordinary user/group side together with Package/Capability SIDs. An early probe that removed inherited ACLs produced `0x80070005`; preserving normal inherited ACLs and adding only the intended AppContainer ACEs restored correct behavior while cross-package writes remained denied.

### R5.5 - Shared runtime capability probe

Status: passed.

`appcontainer_capability_probe.rs` creates two distinct disposable AppContainer profiles (A and B) and derives one private capability named `CodingToolsMcp.Sandbox.RuntimeProbe`. Both profiles register that capability and both child tokens receive it through `SECURITY_CAPABILITIES`.

The disposable ACL model is:

- root: Package A and Package B receive traversal/read-execute access;
- folder A: only Package A receives Modify;
- folder B: only Package B receives Modify;
- shared runtime directory: only the private capability SID receives read/execute.

Runtime result with the private capability:

- A exits 0 and writes its own folder;
- B exits 0 and writes its own folder;
- A cannot cross-write B;
- B cannot cross-write A;
- both A and B can execute the shared helper through the same private capability;
- a control launch using Package A but omitting the shared capability cannot execute the helper and therefore exits with the probe's expected success code for denial.

A well-known `internetClient` capability was also used once as a disposable control while debugging the probe; it passed under the same ACL model, confirming that the capability pipeline itself matched the Windows contract. The final/private test then passed with `CodingToolsMcp.Sandbox.RuntimeProbe`, so production does not need a broad well-known capability or `ALL_APPLICATION_PACKAGES` merely to share runtime read/execute access.

This proves the authorization topology. R5 showed Python/Rust/npm need additional runtime access; R5.6 below validates a bounded private-capability provisioning strategy against the real installed runtimes. Cache, HOME, TEMP/TMP and package-manager write locations still need explicit redirection or folder-scoped grants before production integration.

### R5.6 - Real runtime capability provisioning

Status: passed for executable/runtime access; writable cache/home/temp behavior remains a separate gate.

`appcontainer_broker_probe.rs --runtime-capability-matrix` applies the same stable private capability `CodingToolsMcp.Sandbox.RuntimeProbe` to the real installed Python, Rust and Node/npm runtimes, then runs the commands through the R4 AppContainer pipe/Job Object launcher. The experiment is intentionally reversible: a cleanup mode removes the stable SID before and after every matrix, the matrix uses RAII removal for each temporary grant, and representative runtime files are checked for SID residue after cleanup.

The important provisioning rule is that Scoop's `current` directories are junctions. An ACE on the junction itself does not make DLLs/scripts in the physical version directory accessible. The probe therefore grants shallow traversal on the junction plus inheritable read/execute on the resolved physical version root. Rustup's active toolchain paths are already physical, so only its bounded `bin` and `lib` roots are needed.

Measured candidate runtime material on this host:

| Runtime root | Approximate material |
| --- | --- |
| Python 3.12 physical root | 4,922 files / about 144 MB |
| Rust toolchain `bin` | 23 files / about 360 MB |
| Rust toolchain `lib` | 78 files / about 267 MB |
| npm package root | 2,262 files / about 13 MB |

The final matrix result is:

| Case | Result with private capability | Notes |
| --- | --- | --- |
| Python 3.12 | pass | `-I -S -c` exits 0 and prints `python-capability-ok` |
| rustc | pass | `rustc 1.97.1 ...` exits 0 |
| Cargo | pass | `cargo check --quiet` on a disposable workspace exits 0 using the direct toolchain rustc |
| npm | pass | npm 11.3.0 exits 0 through the physical Node executable |
| Python control without capability | denied | still exits `0xC0000135`, proving the capability ACE is not usable without the capability token |

The successful npm launch exposed a backend-specific normalization requirement. Invoking Scoop `npm.cmd` directly eventually causes Node to canonicalize absolute module paths and `lstat` the drive root, which AppContainer denies. Granting `C:\` recursively would be unacceptable and was not done. The successful narrow form is equivalent to:

```text
<physical node.exe> --preserve-symlinks --preserve-symlinks-main <physical npm-cli.js> ...
```

This keeps access limited to the approved Node/npm runtime roots. The production AppContainer backend therefore needs a command-normalization hook for toolchain launchers such as npm rather than leaking AppContainer-specific rewrites into the generic `exec_command` contract. Other sandbox backends may choose different normalization or none at all.

Shallow grant/remove operations remained bounded: Rust roots were generally tens of milliseconds, while the larger Python and Node/npm physical roots were generally a few hundred milliseconds. No recursive 70k-file ACL scan was used. After the passing matrix, representative Python, rustc, Rust lib, npm launcher and npm CLI paths all reported `has_sid=false`, and a final defensive cleanup completed successfully.

This makes two production strategies viable: provision/cache the private capability ACE on approved concrete runtime roots once, or materialize approved runtimes into an app-managed shared runtime cache that already carries the capability ACL. The latter reduces mutation of user-installed toolchains; the former is now empirically proven to work with bounded roots. Production can support both behind the AppContainer backend without changing the backend-neutral UI/config model.

### R5.7 - Workspace-scoped writable state environment

Status: passed.

Executable/runtime read access is not sufficient for real developer tools: Python needs temporary files, Cargo needs build/cache state, and npm needs cache plus Node/AppContainer temporary state. Granting the real user profile, `%TEMP%`, Cargo home or npm cache would undermine the selected-workspace boundary, so the broker must synthesize a separate writable state tree owned by the workspace sandbox.

The R5.7 probe extends the R4 broker launcher with a real Unicode `CreateProcessW` environment block (`CREATE_UNICODE_ENVIRONMENT`). The block starts from the broker environment, replaces variables case-insensitively, and points writable locations into a disposable per-workspace state root:

```text
state/
  home/
  tmp/
  cache/
  cargo-home/
  cargo-target/
  npm-cache/
  npm-prefix/
  pycache/
```

The broker overrides `TEMP`, `TMP`, `TMPDIR`, `HOME`, `USERPROFILE`, `APPDATA`, `LOCALAPPDATA`, `XDG_CACHE_HOME`, `CARGO_HOME`, `CARGO_TARGET_DIR`, `NPM_CONFIG_CACHE`, `NPM_CONFIG_PREFIX` and `PYTHONPYCACHEPREFIX`. The workspace package SID receives Modify on this backend-managed state root in addition to the selected workspace folder; the shared private runtime capability remains read/execute-only on approved runtime roots.

The passing matrix proves:

| Case | Result | Evidence |
| --- | --- | --- |
| Python tempfile | pass | `NamedTemporaryFile` was created under `state/tmp`; `USERPROFILE` printed as `state/home` |
| Cargo check | pass | command exits 0 and all target artifacts are under `state/cargo-target` |
| npm cache verify | pass | npm exits 0 and verifies `state/npm-cache/_cacache` |
| Node/AppContainer temp | contained | Node's package `AC/Temp/node-compile-cache` was created below redirected `state/home/AppData/Local/Packages/...`, not the real user profile |

An initial npm cache verification exceeded the generic 30-second probe wait even though it had already created cache files. The broker therefore keeps the default 30-second research capture semantics but allows a bounded per-case wait; the successful rerun used a 120-second cap and actually completed in a fraction of a second after startup. Production must continue using the caller's normal timeout/deadline rather than hard-coding a sandbox-specific fixed timeout.

Cleanup guarantees also passed: the successful probe removed its disposable state/workspace directory, registry lookup found zero `CodingToolsMcp.Sandbox.StateEnv` profiles, the stable runtime capability cleanup completed, and representative Python/Rust/Node/npm runtime paths had no remaining research capability SID.

This establishes a critical production contract: an enabled sandbox has two explicit mutable surfaces, not one implicit user profile. The user-selected workspace folder is the project surface; the backend-managed per-workspace state directory is the tool/cache/temp surface. The latter should be created under application-managed data, receive only the workspace package SID, and have an explicit lifecycle policy. Ephemeral temp/session state may be removed when a session ends, while build/cache state may persist per workspace. Real HOME/config/secrets must never be copied wholesale into this state tree.

The environment and state construction are backend responsibilities. AppContainer needs Windows-specific HOME/TEMP/AppData redirection and npm command normalization; another sandbox backend may need different mounts, namespaces or environment rules. These behaviors therefore belong behind the sandbox provider interface rather than in generic `exec_command`.

### R5.75 - Backend-neutral sandbox configuration and UI contract

Status: implemented on the research branch; AppContainer enforcement remains gated as research-only.

Sandbox selection is a workspace-wide execution safety setting shared by MCP and Actions. The persisted model separates whether sandboxing is enabled from which backend is selected:

```text
SandboxConfig {
  enabled: bool,
  backend: string,
}
```

The backend identifier is intentionally a string rather than a closed serialized enum. UI discovery comes from a Rust backend registry that exposes descriptors such as host support, WSL support, experimental status and whether the production enforcement path is ready. This lets later providers be registered without teaching the UI a new hard-coded enum. Candidate future providers can include additional Windows isolation mechanisms, WSL/container-backed execution, or platform-specific sandboxes.

The execution invariant is fail-closed: when `enabled=true`, an unknown, unsupported or not-yet-ready backend blocks command execution. It must never silently fall back to `policy_only`. AppContainer is therefore visible as an experimental backend after its isolation/runtime/state research gates passed, but `enforcement_ready=false` remains in place until the production prepare/launch bridge is wired end-to-end, preventing the UI setting from creating a false security claim.

MCP and Actions both receive the same persisted sandbox selection when their execution contexts are created. A sandbox-setting change is rejected while either service is active, because an already-created context must not continue executing under an older isolation boundary after the UI displays a new one. The settings UI mirrors this rule by locking sandbox controls until both services are stopped; the backend check remains authoritative even if a caller bypasses the UI.

Existing workspace profiles are backward compatible: a missing sandbox object deserializes to `enabled=false` and backend `appcontainer`, preserving the current `policy_only` behavior until the user explicitly enables sandboxing.

### R5.8 - Provider lifecycle and launch-plan abstraction

Status: implemented as a backend-neutral contract; production process launch remains intentionally unwired.

The earlier descriptor-only registry is now extended with a provider lifecycle that reflects what R5.6 and R5.7 actually required. A `SandboxBackend` can prepare a provider-owned `PreparedSandbox`; the prepared object is the lifetime boundary for OS/provider resources and exposes three generic planning surfaces:

- managed state layout and persistence policy;
- command normalization;
- environment overrides.

The generic launch-plan shape contains a normalized executable/argument/cwd request, backend-owned environment overrides and optional managed state metadata. A concrete prepared provider may internally hold AppContainer profile/SID/capability state, WSL/container mount handles, namespace state, or any future platform-specific resources; those details do not leak into `exec_command`.

This split is deliberate. AppContainer proved that npm sometimes needs `npm.cmd` rewritten to physical `node.exe` + npm CLI with symlink-preservation flags, and that HOME/TEMP/cache state needs Windows-specific redirection. A different sandbox may not need either behavior. Therefore generic exec should ask the prepared provider for a launch plan rather than accumulate `if backend == appcontainer` branches.

`PreparedSandbox` is also the future cleanup ownership boundary: when the prepared value is released, the concrete provider can release profiles, capability grants, mounts or broker sessions. A focused unit test uses a fake prepared provider to prove that state metadata, command rewriting and environment overrides compose into one launch plan without generic code knowing the provider implementation.

The public preparation entrypoint still calls the same fail-closed registry check first. Because AppContainer remains `enforcement_ready=false`, the new lifecycle cannot accidentally activate the research backend. R6 will implement AppContainer's concrete prepared object from the proven R5.6/R5.7 behavior, then delegate actual process creation/stdio/job/timeout semantics through that provider before flipping readiness.

### R6 - Production integration

R1-R5 are green, so production integration is proceeding in independently tested stages. `enforcement_ready` remains false until the final `tools/exec` path, post-checks, health checks, runtime provisioning, telemetry and cleanup semantics are all using the same enforced backend.

#### R6.1 - Backend-neutral child lifecycle

Status: passed and committed.

`ExecSession` no longer owns `tokio::process::Child` directly. A backend-neutral `ProcessChild` now owns process lifetime, stdin/stdout/stderr, process id, process-tree containment, wait/try-wait/kill behavior and optional backend lifetime state. Native and WSL execution still enter through the Tokio variant, so this was a behavior-preserving refactor rather than an isolation change.

Focused regressions passed after the refactor: 13 session tests and 20 exec tests, including retained output, timeout handling, post-check capture, Windows batch/PowerShell/Python execution, permission resume and operation deduplication.

#### R6.2 - Raw Windows process bridge

Status: passed and committed.

`ProcessChild` now has a Windows raw-handle variant suitable for `CreateProcessW`. Process handles are owned, waiting is delegated through bounded blocking work, `try_wait` uses a zero-time Windows wait, kill uses `TerminateProcess`, and anonymous pipe parent ends are adapted to Tokio async files.

The Windows process-tree helper also gained a handle-based attach path. This allows the security-critical launch order required by AppContainer: create suspended, assign the already-open process handle to a kill-on-close Job Object, then resume. A Windows-only integration test launches `cmd.exe` through raw `CreateProcessW`, attaches the Job Object before `ResumeThread`, then proves stdout, stderr, exit status and containment through the same `ProcessChild` interface used by normal sessions.

#### R6.3 - Production AppContainer launcher primitive

Status: passed; backend readiness remains false pending generic exec/runtime integration.

A production `tools/sandbox/appcontainer.rs` launcher now owns disposable profile creation, `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`, inheritable stdio pipes, Unicode environment blocks, Windows command-line quoting, suspended `CreateProcessW`, pre-resume Job Object assignment and conversion into `ProcessChild`. `ProcessChild` can retain an opaque backend lifetime guard, so the AppContainer profile is not deleted while the child/session is still alive.

An important Windows lifetime bug was caught during this extraction: `UpdateProcThreadAttribute` retains pointers to `SECURITY_CAPABILITIES` and `SID_AND_ATTRIBUTES`; moving those structures out of scope before `CreateProcessW` produced `ERROR_INVALID_PARAMETER (87)`. The production attribute-list object now owns boxed capability structures at stable addresses for the complete process-creation lifetime.

The production-launcher integration test uses a unique disposable AppContainer profile and temporary directory. It grants read/traverse to the temp root and Modify only to the selected inner workspace, launches a copied test binary through the production launcher, and verifies from inside the child that its token is AppContainer, the workspace is writable, the parent directory is not writable, stdout/exit capture works and Job Object containment is active. The profile is held until child teardown and a post-test registry search reports zero matching profile mappings. Existing session and exec regressions remain 13/13 and 20/20.

#### R6.4 - Backend-neutral prepared process specification

Status: passed.

`exec/runner.rs` now resolves a `PreparedProcessSpec` before materializing a Tokio command. The spec carries the concrete executable, ordinary arguments, optional Windows raw argument tail, host cwd, caller environment mutations, removals, backend-required environment values and whether the invocation was normalized through WSL. Windows `.cmd`/`.bat` handling therefore keeps its exact `cmd.exe /d /s /c` + raw-tail semantics, while `.ps1` is normalized to a concrete PowerShell executable plus ordinary arguments before launcher selection.

Environment ordering is explicitly modeled rather than flattened. The historical contract is caller `env` first, then `remove_env`, then Windows-required Python UTF-8 values. The latter are stored separately as `required_env`, so a caller removal cannot accidentally suppress `PYTHONUTF8=1`, `PYTHONIOENCODING=utf-8` or `PYTHONLEGACYWINDOWSSTDIO=0`. WSL keeps its embedded env/removal invocation and does not receive a host cwd, matching prior behavior.

Two focused neutral-spec tests passed, and the complete exec regression suite passed 22/22, including Windows batch quoting with spaces/special characters, PowerShell UTF-8 behavior, Python Unicode execution, WSL wrapping/path validation, post-checks, timeouts, dedupe and retained sessions.

#### R6.5 - Backend-produced child startup admission

Status: passed.

The startup controller now exposes a generic `start_process_with_permission` path that accepts a backend-created `ProcessChild`. The existing `spawn_with_permission` API remains as a Tokio-command compatibility wrapper, so native/WSL callers retain the same behavior while AppContainer can later create its suspended raw process and still consume the same startup slot, gate timing and diagnostics ownership.

A focused test proved a backend-created `ProcessChild` enters the shared startup permission path with the expected diagnostics, and the complete exec regression suite remained 22/22. This removes the last Tokio-command type dependency from the startup-admission boundary without enabling sandbox execution yet.

#### R6.6 - Prepared AppContainer provider lifetime

Status: provider preparation implemented and focused tests passed; public readiness remains false.

`AppContainerPreparedSandbox` now owns a workspace-scoped provider lifetime: a disposable AppContainer profile, a private capability SID, package-SID access to the selected workspace and managed state, workspace-scoped HOME/TEMP/cache environment overrides, and a registry of command-specific runtime capability grants. Managed persistent state is keyed from a hash of the canonical workspace root beneath application-managed data; focused tests use disposable state roots instead of touching that persistent location.

Runtime provisioning was deliberately moved out of workspace preparation and into per-command `prepare_launch`. This prevents a Git-only workspace from requiring Python/Rust/npm to exist and gives future backends/adapters a clean place to provision only the concrete runtime needed by the resolved command. Workspace-local executables need no external runtime grant; existing external adapters cover Python, Node, npm, Cargo and rustc. Unknown external runtimes fail closed with `SANDBOX_RUNTIME_UNSUPPORTED` rather than receiving a broad filesystem grant.

The npm adapter keeps the R5 result: it resolves the physical Node installation and rewrites npm to `node.exe --preserve-symlinks --preserve-symlinks-main npm-cli.js ...`, with the private capability limited to the required Node/npm roots. Rust resolves through `rustup which` and grants only toolchain `bin`/`lib`; Python/Node account for Scoop junctions by separating traversal of the junction path from inheritable RX on the physical target.

A Windows path-representation bug was caught while testing workspace-local tools: `Workspace::root()` is canonical while callers may provide normal `C:\...` paths. Provider containment now canonicalizes existing executables before deciding whether they are workspace-local, so `C:\...` versus `\\?\C:\...` cannot incorrectly turn a workspace tool into an unsupported external runtime.

Focused provider tests pass 3/3, the production launcher test remains 1/1, the public sandbox suite remains fail-closed, and registry lookup after provider tests reports zero matching disposable `CodingToolsMcp.Sandbox.Workspace` profiles. A dedicated public-entrypoint regression also asserts that `prepare_enabled_backend` still returns `SANDBOX_BACKEND_NOT_READY`; wiring an internal provider implementation therefore does not silently activate it.

#### R6.7 - Two-stage logical and concrete process pipeline

Status: passed.

The command-ordering ambiguity is now explicit rather than implicit. `PreparedSandbox` has two different hooks with different responsibilities: `normalize_logical_command` runs before generic platform normalization, while `prepare_process` runs afterward on the exact `ProcessLaunchSpec` that the OS launcher will receive. AppContainer therefore sees `npm.cmd` early enough to rewrite it to physical Node + npm-cli.js, but still sees the final concrete executable later for runtime provisioning.

The concrete process shape was promoted from a private `exec/runner.rs` struct into sibling-neutral `tools/process_spec.rs`. It carries executable, ordinary arguments, cwd, caller env additions/removals, required env values, optional Windows raw argument tail, and WSL-normalization state. Native Tokio execution still consumes this same type, so sandbox integration does not need a second model or duplicate quoting semantics.

AppContainer now treats logical npm normalization and concrete runtime preparation separately. The concrete stage rejects WSL, canonicalizes workspace-local executables, provisions Cargo/rustc/Python/Node runtime roots only after final launcher selection, and can recognize the npm-cli argument on a normalized Node invocation to retain the narrower Node/npm grant set proven in R5. Unknown concrete external runtimes still fail closed.

Provider lifecycle traits are now crate-internal; only backend descriptors/config remain public to the UI/API surface. This removes an accidental public API leak of internal process-launch types while preserving extension points inside the execution engine.

Validation: the complete sandbox/provider/production-launcher group passes 13/13, the native/WSL exec suite remains 22/22, and the static UI/config contract remains green. No new private-interface warnings remain.

#### R6.8 - Shared process-spec AppContainer launcher

Status: passed.

The production AppContainer launcher now consumes the same `ProcessLaunchSpec` as native/WSL execution. It rejects WSL-normalized specs, requires an explicit host cwd, uses the concrete `program`/`args`, and preserves `windows_raw_arg` verbatim after ordinary quoted arguments. This keeps `.cmd`/`.bat` execution semantics aligned with the native Tokio path instead of rebuilding a second command model inside the sandbox backend.

The Unicode environment block now applies layers case-insensitively in a fixed security-sensitive order: host environment -> caller additions -> caller removals -> required runtime environment -> sandbox-owned state overrides. Sandbox-owned HOME/TEMP/cache values are deliberately last, so a caller cannot point them back to the real user profile by adding or removing the same keys. A focused regression proves sandbox TEMP/HOME win, required environment survives a caller removal when re-added by the required layer, and a remove-only variable remains absent.

The real production-launcher integration test now passes its child-test marker through `ProcessLaunchSpec.env`, proving caller environment additions traverse the same path used by production. Focused AppContainer launcher coverage is 4/4 (child token/isolation, raw-tail preservation, environment precedence, real contained launch) and the native/WSL exec suite remains 22/22. No stale `SandboxCommand` launcher call sites remain.

#### R6.9 - Provider-owned launch and child-held lease

Status: passed; public readiness remains false.

`PreparedSandbox` now owns the final `launch_prepared_process` hook. AppContainer validates that the plan belongs to its backend, launches through the production `CreateProcessW` primitive with the prepared profile/private capability, and attaches the complete `Arc<AppContainerLease>` to the returned `ProcessChild`. `ProcessChild` lifetime guards are now additive rather than a single replaceable slot, so a launcher/profile guard and a higher-level provider lease can coexist safely.

This lifetime ownership is security-critical: the provider lease contains the package-SID workspace/state ACL grants and command-specific private-capability runtime grants. Dropping `PreparedSandbox` immediately after process creation must not remove those grants while the child is still using them. A dedicated integration test launches a copied workspace-local test binary through provider preparation plus the shared startup-permission path, delays the AppContainer child by 300 ms, drops the prepared provider before the delayed write, and still observes a successful workspace write while the unique parent directory remains unwritable. The only surviving ownership during that delay is the `ProcessChild`-held lease.

Validation: focused lease lifetime test passes 1/1; the complete sandbox/provider/launcher group is now 16/16; native/WSL exec remains 22/22; the static backend-neutral contract remains 3/3; post-test AppContainer registry search reports zero matching `CodingToolsMcp.Sandbox.Workspace` profiles.

#### R6.10 - Shared startup probe and loader retry control

Status: passed.

Windows startup resilience is now process-backend neutral. A generic `start_process_with_control` controller accepts any closure that produces `ProcessChild`, acquires the same startup permission/gate, records the same startup diagnostics, probes the child during the existing startup window, recognizes `0xC0000142` DLL initialization failure, records it in the same circuit breaker, applies the same bounded retry delays, and retries through the same backend closure. The historical Tokio `spawn_with_control` entrypoint is now only a compatibility wrapper around this controller.

A deterministic focused regression proves the generic path rather than merely compiling it: a backend-produced test child intentionally exits with `0xC0000142` on the first attempt and succeeds on the second. The controller reports two attempts and one retry delay; the observed first delay is 250 ms. The real AppContainer provider lease-lifetime integration test also now uses this controller and still succeeds, so the raw AppContainer path no longer bypasses native Windows startup resilience.

Validation after the refactor: complete sandbox/provider/launcher suite 16/16, native/WSL exec suite 22/22, static sandbox contract 3/3, and AppContainer workspace-profile registry search reports zero matches after tests. Existing repository warnings remain unchanged.

#### R6.11 - Internal end-to-end sandbox orchestration

Status: passed; public execution remains gated.

There is now one crate-internal orchestration helper for a prepared sandbox command. It applies provider logical normalization, calls the same generic Windows/WSL process-spec builder used by native exec, passes the resulting `ProcessLaunchSpec` through provider concrete-runtime preparation, then enters the shared startup admission/loader-retry controller and finally invokes the provider-owned production launcher. `PreparedSandbox` is now `Send + Sync`, so this prepared provider can safely be borrowed across the asynchronous startup-control path.

The generic process-spec builder is exposed only crate-internally from `exec/runner.rs`; existing `ExecSpec` handling delegates to it, so the new sandbox orchestration did not fork Windows batch quoting, PowerShell normalization, WSL wrapping, cwd conversion or environment layering. AppContainer launch also consumes the plan's own backend environment overrides rather than reaching around the plan to provider internals.

The existing lease-lifetime AppContainer integration test was converted from hand-wired stages to this orchestration helper. It still launches a real AppContainer child, preserves the delayed-write child-held lease proof, and therefore now validates logical hook -> shared process normalization -> concrete provider preparation -> generic loader controller -> production AppContainer launcher -> `ProcessChild` in one path. Validation: focused e2e 1/1, complete sandbox group 16/16, native/WSL exec 22/22, static contract 3/3, and disposable workspace-profile registry search reports zero matches.

#### R6.12 - Trusted zero-grant Windows system runtimes

Status: passed; public readiness remains false.

Generic Windows normalization can now safely terminate at trusted system runtimes without introducing new filesystem grants. Direct production-launcher probes established that `C:\Windows\System32\cmd.exe` can execute a workspace `.cmd` script inside AppContainer with no private runtime ACL, and the runner-selected PowerShell Core runtime (`C:\Program Files\PowerShell\7\pwsh.exe` on the validation host) can likewise execute successfully without a private runtime ACL. The provider therefore treats these as identity-validated zero-grant runtimes rather than provisioning broad Windows or Program Files access.

`cmd.exe` identity is resolved from `GetSystemDirectoryW`, not PATH or caller-controlled environment values. PowerShell identity is shared directly from the existing runner detection, so the provider does not maintain a second discovery policy. Bare `cmd.exe` is pinned to the real system executable; the selected PowerShell name/path is pinned to the same executable chosen by native execution. Canonical path comparison rejects same-name binaries from arbitrary directories. A focused spoof regression creates fake `cmd.exe` and fake selected-PowerShell binaries and confirms neither is accepted as trusted.

Provider-level end-to-end coverage now exercises the complete internal orchestration with real workspace `.cmd` and `.ps1` files: logical provider hook -> shared platform normalization -> trusted concrete-runtime preparation -> shared startup/loader controller -> production AppContainer launcher -> child-held provider lease. Both scripts succeed after the prepared provider is dropped, proving the child-held lease remains sufficient while `cmd.exe`/PowerShell opens the workspace script. Unknown external runtimes remain fail-closed.

Validation: AppContainer provider focused suite 7/7; complete sandbox/provider/launcher suite 21/21; native/WSL exec suite 22/22; static sandbox contract 3/3; `git diff --check` passes; AppContainer workspace-profile registry search reports zero matches after tests. Existing repository warnings remain unchanged.

Next integration gate: integrate prepared sandbox process creation into the existing `exec_command` session lifecycle instead of creating a parallel execution stack. The same session registry, output streaming, stdin, timeout/kill behavior, post-check sequencing, startup diagnostics and result telemetry must work for native and AppContainer children. Keep `enforcement_ready=false` until that public execution path is proven end-to-end and no enabled configuration can silently fall back to policy-only execution.

#### R6.13 - Shared exec session lifecycle and post-check containment

Status: passed; public backend selection remains disabled.

Main-command and post-check process creation now share one backend-neutral `CommandExecutionBackend` starter. Native execution remains the production default for this stage, while a prepared sandbox backend is represented by an `Arc<dyn PreparedSandbox>` so the provider can outlive the main launch and remain available to asynchronous post-checks. The existing `ExecSession` still owns the resulting `ProcessChild`; no second session registry, output reader, stdin channel, timeout monitor or result pipeline was introduced.

The former Windows loader retry loop inside `run_command` was removed in favor of the generic R6.10 startup controller. `run_command` now acquires active-session capacity, asks the selected backend for one controlled `ProcessChild`, inserts that child into the unchanged session lifecycle, starts the existing readers/exit waiter, and passes a clone of the backend into the lifecycle monitor. Post-checks use the same backend starter, so enabling a sandbox can no longer sandbox the main command while silently verifying on the host. The obsolete native-only `spawn_with_permission` wrapper was removed.

Two Windows integration regressions exercise a real prepared AppContainer backend through `run_command`, not just the lower-level provider launcher. The first makes both the main workspace `.cmd` and its post-check attempt writes in the workspace parent; those writes are denied under AppContainer, while any accidental native fallback would create the marker and force a non-zero exit. Both main execution and verification succeed and neither outside marker exists. The second proves the unchanged Session stdin path by piping a value into an AppContainer `.cmd`, verifies startup diagnostics survive the shared path, then runs a sleeping AppContainer `.ps1` with a 250 ms process timeout and observes the existing `process_timeout` termination semantics without the completion marker.

Validation: complete exec suite 24/24, complete sandbox/provider/launcher suite 21/21, static sandbox contract 3/3, `git diff --check` passes, and AppContainer workspace-profile registry search reports zero matches after tests. Warning count returns to the existing repository baseline; the temporary `Sandbox` dead-code warning disappeared once the lifecycle integration tests exercised it.

Next integration gate: make public `exec_command` select a prepared backend when sandboxing is enabled, while preserving fail-closed configuration semantics and truthful result telemetry. The executor must prepare the configured backend before any child process is created, keep native execution only for explicitly disabled sandboxing, mark successful sandbox results as enforced/AppContainer rather than `policy_only`, and ensure startup/preparation failures are surfaced rather than falling back. Keep the descriptor `enforcement_ready=false` until that public-path test is implemented; only then consider the readiness flip as a separate reviewed stage.

#### R6.14 - Public boundary selection and truthful telemetry contract

Status: passed; readiness remains false.

`exec_command` now derives a `CommandExecutionBoundary` from the persisted sandbox configuration before any native diagnostic or child launch. Disabled sandboxing maps to `PolicyOnly`; enabled sandboxing must pass the existing registered-backend support/readiness preflight. The actual provider is deliberately prepared only after operation admission decides a new process is required, so dedupe/reattach does not create a disposable AppContainer profile or mutate ACL state merely to reconnect to an existing session.

The legacy in-process native diagnostic path is now reachable only when the selected boundary is `PolicyOnly`. An enabled sandbox can therefore no longer pass readiness preflight and then bypass the sandbox through the diagnostic optimization. On a new execution, the selected boundary prepares exactly the configured backend; a configuration identity mismatch fails closed with `SANDBOX_CONFIGURATION_CHANGED` and `fallback_allowed=false` rather than selecting Native.

Result metadata is now boundary-owned instead of hard-coded at individual call sites. Native results remain `sandbox_enforced=false` / `execution_boundary=policy_only`. A successfully started sandbox session is marked `sandbox_enforced=true`, carries `sandbox_backend=<backend id>`, and reports that backend id as the execution boundary. A sandbox startup failure remains `sandbox_enforced=false`, carries the attempted backend, and reports `execution_boundary=sandbox_start_failed`; it never claims enforcement before a sandbox child exists. Dedupe/reattach uses the same metadata contract. `exec_health_check` intentionally retains its existing native probe behavior and readiness preflight in this stage.

Focused tests prove disabled selection is policy-only, enabled AppContainer still returns `SANDBOX_BACKEND_NOT_READY` with no fallback while readiness is false, success/start-failure telemetry cannot be confused, and the real public `exec_command` path does not execute a workspace marker script when AppContainer is enabled but not ready. Validation: complete exec/backend suite 28/28, sandbox/provider/launcher suite 21/21, static sandbox contract 3/3, `git diff --check` passes, and AppContainer profile registry search reports zero matches after tests.

Readiness audit after this stage found one remaining production compatibility mismatch against gate 6: R5 proved Git installed under Program Files works in AppContainer without a private runtime grant, but the production provider still treats that concrete external executable as unsupported. The next stage must add an exact-identity zero-grant Git adapter and prove it through the production provider. Readiness must not flip merely because public selection is now wired.

#### R6.15 - Exact-identity zero-grant Git runtime

Status: passed; readiness remains false pending the final gate audit.

The production AppContainer provider now recognizes the exact Git executable selected by the normal Windows PATH resolver as a zero-grant runtime, matching the R5 compatibility result for the Program Files Git installation. The trust rule is identity-based rather than filename-based: bare `git`/`git.exe` is pinned to the resolved Git installation, and an explicit path is accepted only when it resolves to that same existing file. A fake same-name executable in another directory is rejected.

A production-provider integration test launches the resolved Git executable with `--version` through logical normalization, concrete process preparation, shared startup control and the real AppContainer launcher. The child remains Job Object-contained, starts without a private runtime ACL grant, exits 0 and emits Git version output. This closes the R6.14 compatibility mismatch without broadening Program Files access or adding a persistent capability ACE for Git.

Validation: complete sandbox/provider/launcher suite 22/22, complete exec/backend suite 28/28, static sandbox contract 3/3, `git diff --check` passes, and the post-test AppContainer workspace-profile registry search reports zero matches. Existing repository warnings remain unchanged.

Next gate: re-audit all eight validation gates against the production provider and shared exec lifecycle. Do not flip `enforcement_ready` merely because the known Git gap is closed; any missing production-path evidence must be implemented first.

#### R6.16 - Health-check backend selection and truthful runtime telemetry

Status: passed; readiness remains false pending the final validation-gate audit.

`exec_health_check` no longer performs a native child probe after only checking sandbox readiness. It now resolves the same `CommandExecutionBoundary` as `exec_command`, resolves the probe command, prepares the selected backend, and passes that backend into the unchanged `run_command` session lifecycle. The returned probe snapshot receives the same boundary-owned metadata, so a disabled sandbox reports `policy_only` while a future enabled/ready sandbox health probe will be launched by and reported as that backend rather than silently escaping to Native.

`server_info` no longer hard-codes filesystem sandbox availability/enforcement to false. It derives the selected backend's workspace support and `enforcement_ready` state, reports the persisted `enabled`/`backend` selection, marks an enabled-but-unavailable backend as `sandbox_unavailable`, and reports `workspace_exec.available=false` instead of implying that policy-only fallback is possible. When the selected backend is ready, `available` becomes true independently of enablement, while `enforced` becomes true only when the workspace has explicitly enabled that ready backend.

Focused Rust coverage now exercises the default disabled health probe through the real public tool dispatch and verifies captured stdout/stderr plus `sandbox_enforced=false` / `execution_boundary=policy_only`. A readiness-relative server-info test derives its expected values from the AppContainer descriptor, so it remains valid across the later readiness flip rather than baking in today's false value. Static contract checks ensure the health implementation contains configured boundary preparation and no longer contains a hard-coded Native health child.

Validation: complete exec/backend suite 30/30, complete sandbox/provider/launcher suite 22/22, static sandbox contract 3/3, Node cross-runtime tool/health contract 4/4, `git diff --check` passes, and the post-test AppContainer workspace-profile registry search reports zero matches. Repository warning count remains at the existing baseline.

Next gate: produce a final evidence matrix for validation gates 1-8. Any gate that is only covered by an early research probe, rather than the production provider/shared lifecycle where that distinction matters, must be closed before changing `enforcement_ready`.

#### R6.17 - Production package identity, mutable workspace and descendant containment

Status: passed; validation gates 1, 2 and 4 now have production-launcher evidence.

The production launcher regression now validates the AppContainer package identity itself instead of only checking `TokenIsAppContainer`. The launcher converts the exact package SID returned by the profile it created to string form and passes it only to the identity-focused probe. Inside the sandboxed command, `GetTokenInformation(TokenAppContainerSid)` reads the real token's `TOKEN_APPCONTAINER_INFORMATION`; the returned package SID must equal the launcher's expected SID. This closes the gap between “some AppContainer” and the intended per-workspace AppContainer identity.

The same real sandboxed child now proves selected-workspace mutability by creating and deleting a file inside the workspace. It still creates a durable inside marker and verifies that an attempted parent/sibling write fails at the OS boundary. The launcher verifies the inside marker exists, the deleted file remains absent, and the outside marker was never created.

The production child also launches the same copied test executable as an ordinary grandchild. The expected package SID environment is inherited, so the grandchild independently proves it has the same AppContainer package SID, writes inside the workspace and fails to write outside. The parent requires the grandchild to exit successfully, while the launcher verifies the grandchild inside marker exists and its outside marker does not. This brings the descendant-containment evidence from the earlier research example into the production launcher path.

The child probe remains reusable by provider-lifetime tests: exact SID equality is asserted only when the launcher explicitly supplies an expected SID, while every sandbox child still checks `TokenIsAppContainer`. This preserves provider abstraction boundaries without weakening the dedicated identity regression.

Validation: focused production launcher and provider-lifetime probes pass, complete sandbox/provider/launcher suite 22/22, complete exec/backend suite 30/30, static sandbox contract remains green, and the post-test AppContainer workspace-profile registry search reports zero matches. Existing warning count remains at the repository baseline.

Next gate: move multi-folder isolation from the R5 capability research probe into the production provider. Two independently prepared workspace identities must each modify their own folder while both directions of sibling workspace access remain denied.

#### R6.18 - Production multi-workspace isolation

Status: passed; validation gate 7 now has production-provider evidence.

A new provider regression creates two sibling workspaces under the same host parent and independently calls the production AppContainer provider for each one. Each prepared provider therefore owns its own AppContainer profile/package SID, workspace Modify ACL lease, writable state tree and private runtime capability. Both prepared providers remain alive simultaneously during the test so the result exercises the real multi-workspace authorization topology rather than sequential cleanup side effects.

Workspace A runs a workspace-local `.cmd` through `start_prepared_sandbox_command`; it must create an A-local marker and then attempts to create a marker in workspace B. Workspace B runs the symmetric command. Each process is Job Object-contained, each selected-workspace write succeeds, both commands exit 0 only when the cross marker is absent, and the host-side assertions confirm neither A-to-B nor B-to-A marker exists.

This is stronger than relying on path-policy heuristics: the scripts explicitly contain the sibling paths and reach the real Windows process launcher. The denial is produced by the package-SID ACL boundary while the other workspace's provider and ACL grant are concurrently active. If the two workspaces accidentally shared an effective sandbox identity, the sibling grant would make the cross marker writable and the regression would fail.

Validation: focused production multi-workspace provider test passes, complete sandbox/provider/launcher suite 23/23, complete exec/backend suite 30/30, static sandbox contract 3/3, `git diff --check` passes, and the AppContainer workspace-profile registry search reports zero matches after the suite. Existing warning count remains at the repository baseline.

Next gates: strengthen validation gate 5 with AppContainer-specific stderr and explicit session cancellation evidence, then run the current production provider through the gate-6 runtime matrix for Python, Node, npm and Cargo/Rust rather than relying only on the earlier broker probes.

#### R6.19 - Shared AppContainer session stderr and explicit cancellation

Status: passed; validation gate 5 now has shared-lifecycle evidence for stdin, stdout, stderr, timeout, cancellation and Job Object containment.

The existing AppContainer `ExecSession` regression now emits a dedicated stderr marker from the sandboxed stdin command and verifies that the unchanged session reader captures it independently from stdout. This supplements the already passing stdin round-trip, startup diagnostics and process-timeout assertions without adding any sandbox-specific stream transport.

The same regression now starts a long-running AppContainer PowerShell command through `run_command` with the ordinary retained-session behavior. The command emits a ready marker before sleeping, returns a live `session_id`, reports `process_tree_contained=true`, and would create a completion marker only after the sleep. The test then calls the existing `kill_session_async` path against that session, requires the process to stop, and verifies the completion marker was never created. Cancellation therefore uses the same session registry and Job Object-backed child lifecycle as Native execution; there is no AppContainer-only termination path.

Combined with R6.13's sandboxed stdin/timeout/post-check coverage and R6.17's descendant token containment, validation gate 5 now has direct shared-lifecycle evidence for stdin, stdout, stderr, exit/finalization behavior, process timeout, explicit cancellation and process-tree containment.

Validation: focused AppContainer lifecycle regression passes, complete exec/backend suite 30/30, complete sandbox/provider/launcher suite 23/23, static sandbox contract 3/3, `git diff --check` passes, and the AppContainer workspace-profile registry search reports zero matches after the suite. Existing warning count remains at the repository baseline.

Next gate: run Python, Node, npm and Cargo/Rust through the current production provider (`prepare_with_state_root` plus `start_prepared_sandbox_command`) so validation gate 6 no longer depends on the earlier research broker matrix.

#### R6.20 - Production runtime compatibility matrix

Status: passed; validation gate 6 now has production-provider evidence for the representative Windows toolchain matrix.

The production provider regression now executes the current PATH-selected Python, Node, npm, rustc and Cargo toolchains through `prepare_with_state_root` and `start_prepared_sandbox_command`. Every case reaches the real AppContainer launcher, remains Job Object-contained and must exit successfully. Cargo performs a real `cargo check` against a temporary workspace project rather than only printing its version, which also exercises the sandbox-owned Cargo state and target directories.

The matrix exposed a Windows uv-managed virtual-environment detail that the earlier broker probes did not cover. The selected Python executable is a venv launcher whose concrete base runtime lives under uv's managed Python tree. Resolving the launcher directory alone is insufficient: the AppContainer identity also needs read/execute access to the base runtime tree. The provider now detects `pyvenv.cfg`, recognizes uv-managed environments, preserves the original venv launcher as the process entrypoint and grants only the concrete venv/base runtime roots needed for execution. This keeps `sys.prefix` and `sys.executable` bound to the selected virtual environment instead of silently falling back to the base interpreter.

The Python regression runs in isolated mode (`-I`) and, when the PATH-selected interpreter is a venv, explicitly asserts both venv prefix and executable identity before printing its success marker. The test deliberately does not use Python 3.11 `-S`, because that option suppresses `site` initialization and therefore suppresses the venv prefix behavior being validated. Node executes JavaScript, npm executes `--version`, rustc executes `--version`, and Cargo successfully checks a generated Edition 2024 crate using the provider-owned state layout.

Validation: focused runtime matrix passes; complete sandbox/provider/launcher suite 24/24; complete exec/backend suite 30/30; static sandbox contract 3/3; `git diff --check` passes; post-test AppContainer workspace-profile registry search reports zero matches. Existing repository warning count remains at baseline.

Next gate: audit the original validation-gate wording against the now-production-backed evidence set, close any remaining structured fail-closed/startup-error gap if present, and only then consider changing `enforcement_ready`.

#### R6.21 - Final validation-gate closure before readiness

Status: passed; all eight original validation gates now have direct production/shared-path evidence while `enforcement_ready` remains false.

Validation gate 3 is now tested against the real production AppContainer child for all three destructive outside-tree operations named by the specification. The host creates two existing files outside the selected workspace before launch. Inside the sandbox, the command attempts an unrecognized direct filesystem create/write, replacement of one existing outside file, and deletion of the other. All operations are rejected by the OS boundary. After the child exits, the host verifies the new outside marker was never created, the replacement fixture still contains its original bytes, and the deletion fixture still exists unchanged. This evidence is independent of command-text policy heuristics because the filesystem calls execute inside the copied Rust test command.

Validation gate 8 now has deterministic direct regressions for each required startup failure class. An invalid AppContainer profile request returns `SANDBOX_PROFILE_CREATE_FAILED`; an ACL request targeting a missing path returns `SANDBOX_ACL_GRANT_FAILED`; and a real prepared AppContainer launch of a missing executable returns `SANDBOX_PROCESS_CREATE_FAILED`. Every error is categorized as security, is non-retryable, identifies the AppContainer backend, reports the appropriate prepare/launch status, and includes `fallback_allowed=false`. No failure path starts an unsandboxed replacement process.

Final pre-readiness evidence matrix: gate 1 exact package SID is covered by R6.17; gate 2 selected-folder write/delete by R6.17; gate 3 outside write/replace/delete by R6.21; gate 4 descendant containment/outside denial by R6.17; gate 5 shared stdin/stdout/stderr/exit/timeout/cancellation/Job Object behavior by R6.13 and R6.19; gate 6 cmd/PowerShell/Git/Python/Node/npm/Cargo/Rust compatibility by R6.12, R6.15 and R6.20; gate 7 independent multi-workspace identities by R6.18; gate 8 structured fail-closed profile/ACL/process startup failures by R6.21.

Validation: focused gate-8 failure regressions 3/3 and focused production isolation regression pass; complete sandbox/provider/launcher suite 27/27; complete exec/backend suite 30/30; static sandbox contract 3/3; `git diff --check` passes; post-test AppContainer workspace-profile registry search reports zero matches. Existing repository warning count remains at baseline.

Next step: change only the AppContainer readiness gate and readiness-relative tests, then prove the enabled public `exec_command`, health/telemetry surfaces and full regression suite actually select the production AppContainer boundary before reporting `sandbox_enforced=true`.

#### R6.22 - Production readiness enabled

Status: passed; Windows AppContainer now advertises `enforcement_ready=true` after the eight-gate production/shared-lifecycle matrix passed.

The readiness change is intentionally small. The AppContainer backend descriptor now reports production enforcement readiness on supported Windows hosts; enabled configuration therefore selects `CommandExecutionBoundary::Sandbox { backend_id: "appcontainer" }` and public provider preparation is allowed. Disabled configuration remains `policy_only`, unknown backends remain fail-closed, and the provider still owns all concrete profile, ACL, runtime-grant, process-launch and cleanup work.

The former public readiness-failure regression has been replaced with a real public `exec_command` end-to-end test. With sandboxing explicitly enabled, a workspace-local batch command launches through the production provider, writes its marker inside the selected workspace, exits successfully, remains Job Object-contained and returns `sandbox_enforced=true`, `sandbox_backend="appcontainer"` and `execution_boundary="appcontainer"`. This proves the public metadata is attached to a process that actually started under the sandbox rather than being inferred from configuration alone.

`exec_health_check` also has an enabled AppContainer regression. The health probe now succeeds through the same configured boundary while retaining stdout/stderr capture and reports `sandbox_enforced=true` / `execution_boundary="appcontainer"`. The readiness-relative `server_info` regression independently confirms the enabled filesystem-sandbox and workspace-exec telemetry become available/enforced only when both host support and backend readiness are true; disabled workspaces continue reporting `policy_only` without enforcement.

Readiness=true validation: focused registry, boundary-selection, public-exec and enabled-health regressions all pass. The complete sandbox/provider/launcher suite passes 27/27, including the production runtime matrix; the complete exec/backend suite passes 31/31, including public exec, enabled health and server-info telemetry; static sandbox settings pass 3/3; Node cross-runtime tool/health contracts pass 4/4; and the AppContainer workspace-profile registry search again reports zero matches. The first full sandbox invocation encountered a transient Windows `os error 32` while Cargo attempted to replace a locked compiler object before tests started; the same unchanged suite was reattached/re-run after the lock cleared and passed 27/27, so this was a build-artifact contention event rather than a sandbox regression.

Scope note: this readiness result covers the workspace-scoped AppContainer contract and the eight validation gates above. The broader historical task 4.2 additionally calls for a caller-provided arbitrary external-authorization model plus its full read/path-bypass acceptance matrix; that broader capability is not claimed complete by this readiness flip. Job Object task 4.3 is independently satisfied by the production pre-resume attach, timeout and explicit cancellation evidence.

#### R7.2 - Native Docker and Podman OCI backends

Status: implemented as production sandbox backends; AppContainer remains the default selection. Docker Sandboxes (`docker_sbx`) remains a separate microVM product, not a substitute for native `docker` / `podman`.

The sandbox registry now includes `docker` and `podman` beside AppContainer, Docker Sandboxes, and WSLC. Both use the same ephemeral OCI launch shape: discover the host CLI, require a live engine (`docker info` / `podman info`), inspect-or-pull the configured image, then `run --rm -i --name ... --network <configured> --security-opt no-new-privileges`. Workspace is mounted read/write at `/workspace`; explicit grants are mounted at `/ctmcp/grants/*` with `:ro` preserved. Ordinary SMB/UNC grants remain fail-closed; WSL UNC is allowed. `--network host` is rejected as a namespace escape (`SANDBOX_OCI_NETWORK_FORBIDDEN`). Cancellation force-removes the named container (`docker rm -f` / `podman rm -f`), so the container PID namespace is the process-tree boundary (`process_tree_contained=true`).

Options stay backend-neutral in the shared map: `docker.image` / `docker.network` and `podman.image` / `podman.network` (defaults `ubuntu:24.04` and `none`). Desktop Software management can install Docker Desktop or Podman through WinGet/Homebrew, but never starts the engine or runs `podman machine`. Node and Desktop share the same backend ids and portable-command resolution so Windows host executables are rejected before a host process session is created.

Live opt-in validation uses `CTMCP_TEST_DOCKER=1` / `CTMCP_TEST_PODMAN=1` with `alpine:3.20` as a temporary test image.

#### R7.1 - Microsoft WSL Containers (`wslc`) production backend

Status: implemented and validated as a third sandbox backend; AppContainer remains the default backend selection.

The sandbox registry now includes `wslc` alongside AppContainer and Docker Sandboxes. Persisted sandbox configuration remains backend-neutral: provider-specific settings live in the shared `options` map and the settings UI renders option fields from backend descriptors rather than hard-coding WSLC controls. The WSLC descriptor exposes an OCI image option (`wslc.image`, default `ubuntu:24.04`) and a network option (`wslc.network`, default `none`).

The production provider discovers `wslc.exe`, checks runtime health with `wslc version`, verifies or pulls the configured image during provider preparation, and then launches one ephemeral container per command with `wslc run --rm -i --name ...`. The selected workspace is mounted read/write at `/workspace`; explicit external grants are mounted at private `/ctmcp/grants/*` paths with read-only grants receiving `:ro`. Ungranted host directories are not mounted. Network-backed/UNC grant paths are rejected before canonicalization, broader writable grants cannot erase a nested read-only boundary, and all failures remain fail-closed with `fallback_allowed=false`.

Command lifecycle stays behind the existing `PreparedSandbox` abstraction. Bare commands resolve inside the selected Linux image instead of the Windows host PATH; Windows-only executables are rejected. Environment additions are passed with native WSLC `-e` flags, explicit removals are applied inside the container with `env -u`, and the working directory is translated from its mounted host path. Each launched container owns a unique name and a backend lifetime guard. Session cancellation and timeout use the ordinary `ExecSession` lifecycle plus a provider kill hook that force-removes the container with `wslc remove -f`, so the whole container process tree is terminated rather than leaving a detached descendant.

The isolation-first network default was validated directly against the installed WSLC runtime: `wslc run --network none` produced `HostConfig.NetworkMode="none"`. Users can opt into `bridge` or another named WSLC network through the backend option when a command needs networking. Image contents are intentionally not assumed: the default Ubuntu image is only a safe generic default, while Python/Node/Rust workflows can select a purpose-built development image without changing the generic exec path.

Live opt-in validation (`CTMCP_TEST_WSLC=1`) uses `alpine:3.20` only as a temporary test image. Provider E2E verifies stdin/stdout/environment propagation, workspace writes, read-only external denial, writable external grants, absence of an unmounted sibling path, and explicit session cancellation. Public `exec_command` E2E verifies the full registry -> prepare -> run -> session -> telemetry path and requires `sandbox_enforced=true`, `sandbox_backend="wslc"`, `execution_boundary="wslc"` and `process_tree_contained=true`. Both live tests pass with `wslc.network=none`; post-test `wslc list` and `wslc image list` are empty after cleanup.

Regression validation after integration: complete sandbox/provider suite 46/46, complete exec/backend suite 30/30, static sandbox settings 3/3, Svelte diagnostics 0 errors / 0 warnings, and `cargo clippy --no-default-features --all-targets` exits 0 with the repository's existing warning baseline.

#### R7.2 - WSLC health and server telemetry proof

Status: passed with the installed WSLC runtime.

The existing production `exec_health_check` path required no backend-specific code change: because WSLC is a portable Linux sandbox target, the generic health probe selects `sh`, prepares the configured `wslc` backend, launches the probe in a real ephemeral container, and attaches the normal boundary metadata. An opt-in live regression now requires a successful health result with stdout/stderr capture plus `sandbox_enforced=true`, `sandbox_backend="wslc"`, `execution_boundary="wslc"` and `process_tree_contained=true`.

`server_info` likewise required no production special case. A dedicated regression enables WSLC in the persisted runtime selection and verifies both `filesystem_sandbox` and `workspace_exec` report the selected backend as available/enforced with boundary `wslc`. This fixes the expected public telemetry contract in tests so later refactors cannot silently regress to policy-only or another backend label.

Live focused validation uses `alpine:3.20` with the existing opt-in environment gate. The health probe passed through a real WSLC container, the server-info telemetry assertions passed, post-test `wslc list` was empty, and the temporary Alpine image was removed.

#### R7.3 - Node Agent fail-closed sandbox seam

Status: passed; Node Agent sandbox configuration and telemetry are present, while all Node sandbox transports intentionally remain not-ready until implemented and validated.

The pure-Node runtime now has the same backend-neutral sandbox configuration shape needed by the desktop runtime: `enabled`, string backend id, explicit read-only/modify external path grants, and provider option key/value pairs. Legacy Node configurations continue to migrate to sandboxing disabled with backend id `appcontainer`; environment overrides can select a backend and WSLC image/network values without changing the persisted schema shape. The management configuration store persists and exposes the sandbox selection, hot-applies it only when all process admission lanes, pending operations and retained sessions are idle, and defers changes while process work is active.

Node `server_info` now reports filesystem-sandbox and workspace-exec availability from a Node-local backend registry. AppContainer, Docker Sandboxes and WSLC are recognized as backend ids, but their Node descriptors remain `enforcementReady=false` in this phase. An enabled unknown, unsupported or not-ready backend therefore returns a structured security error with `fallback_allowed=false` before command resolution or host process creation. A direct regression enables Node WSLC while it is still not-ready and proves `exec_command` returns `SANDBOX_BACKEND_NOT_READY` with zero process sessions created. Disabled legacy execution remains policy-only and existing process results now expose explicit sandbox boundary metadata.

Validation: direct TypeScript compilation passes; focused config/process lifecycle tests pass 31/31; management tests pass 23/23 including idle sandbox hot-apply and active-session deferral; `git diff --check` passes. A broader Node test discovery run exposed only the existing production-WSS credential requirement plus the pre-existing `folderIsolation` expectation that conflicts with current session-id auto-routing. Running that isolation test against unchanged `main` reproduces the same failure, so it is not attributed to this sandbox seam.

Next step: implement the pure-Node WSLC transport behind this seam, then flip only the Node `wslc` descriptor to ready after live container, cancellation, post-check and health/telemetry coverage. AppContainer and Docker Sandboxes must remain fail-closed in Node until a shared native implementation or equivalent validated transport exists.

#### R7.4 - WSLC managed-session mount quota resilience

Status: passed for the Rust production backend; pure-Node provisioning remains fail-closed pending a native or pre-provisioned session-storage path.

Live WSLC 2.9.4 testing exposed a preview-runtime lifetime limit that is not visible from ordinary container cleanup: a session accepts at most 15 volume attachments, and removing an ephemeral `--rm` container does not return those attachments to the session budget. The sixteenth mount fails with `0x8007000e`. Resetting the process-global default WSLC session would clear the quota, but that would interfere with unrelated user containers and is therefore not an acceptable product strategy.

The Rust backend now owns a dedicated named WSLC session per canonical workspace identity. Its storage lives under the application-managed sandbox data directory, keyed by a SHA-256 digest of the workspace path. The backend creates or re-enters that storage through `IWslcSessionManager`, runs image inspection/pull and every container command with the explicit session name, and scopes container cancellation to the same session. On this WSLC build custom COM sessions reliably initialize with NAT session transport; command-level network isolation is still enforced independently by `wslc run --network <configured>`, whose default remains `none`.

Each launch reserves only the number of mounts needed while its CLI process is being created. The coordinator tracks the accumulated mount budget and, before a launch would exceed 15, blocks new launch reservations, checks the named session with `wslc list -q` until no container remains, terminates the old custom session, and re-enters the same storage under a new session name. Re-entering the same storage preserves the pulled image cache while resetting the mount quota. Retained `ExecSession`/`ProcessChild` objects are deliberately not treated as proof that a container is still active, because session results can outlive the actual WSLC container; the WSLC session itself is the source of truth for safe rotation.

The COM owner teardown also requires interface lifetime ordering: `IWslcSession` and `IWslcSessionManager` must be released before the thread calls `CoUninitialize`. Reversing that order produced a reproducible `STATUS_ACCESS_VIOLATION` during test teardown; reordering the owned fields eliminated the crash.

Live validation uses three mounts per command and deliberately executes a sixth command after five commands have consumed exactly 15 attachments. The sixth command transparently rotates the product-owned session and succeeds. The same regression continues to prove workspace writes, read-only/writable external grants, hidden-path absence, stdin/stdout behavior and explicit cancellation. Focused WSLC tests pass 4/4 without the live opt-in and the opt-in live provider regression passes 1/1; post-test session inspection shows no product-owned session or container residue.

The pure-Node follow-up found an important transport boundary. The documented `wslc system session enter <storage-path>` CLI can enter an existing session storage but cannot create fresh storage, and `wslc system session` exposes no `create`/`init` subcommand. Therefore Node must not silently use the global default session to claim production readiness. Node WSLC can become ready only after it has a validated way to obtain product-owned session storage (for example a shared native provisioner or explicitly provisioned storage); otherwise it must remain fail-closed.

#### R7.5 - Node Agent WSLC transport with provisioned session storage

Status: transport passed live enforcement testing when explicit WSLC session storage is provisioned; automatic pure-Node storage provisioning remains unavailable.

The Node Agent now has a real WSLC execution transport instead of a metadata-only boundary. `exec_command`, buffered post-checks and `exec_health_check` resolve portable Linux commands, mount only the workspace plus explicit external grants, preserve read-only grants, redirect execution through `wslc`, expose backend cancellation, and report `sandbox_enforced=true`, backend `wslc`, execution boundary `wslc` and process-tree control `wslc_container` only after the transport is actually active. Windows-only launch forms are rejected by the portable command resolver rather than being passed to a Linux container.

Because the WSLC CLI cannot create fresh custom session storage, Node requires an explicit `wslc.session_storage` option (or `CTMCP_WSLC_SESSION_STORAGE`) pointing at an existing local WSLC session storage directory. Missing storage fails closed with `SANDBOX_WSLC_SESSION_STORAGE_REQUIRED` before a process session is created. `server_info` distinguishes implementation capability from runtime availability: the WSLC descriptor can be enforcement-ready while `available=false`, `enforced=false` and workspace boundary `sandbox_unavailable` until storage is configured. AppContainer and Docker Sandboxes remain not-ready in the pure Node runtime.

For each Node command, the transport enters the provisioned storage under a unique product-owned named session, performs image inspect/pull in that session, runs and cancels the container with the same explicit session name, then terminates the session before releasing the storage lease. An in-process exclusive lease rejects concurrent use of the same storage instead of sharing it. Main-command cleanup is awaited before post-check preparation, and each post-check gets a fresh named session. Startup failures also tear down the session owner. No Node sandbox command uses the process-global default WSLC session.

Live validation used a temporary storage directory provisioned only by a test-only Rust COM hook, then removed both the hook and storage before commit. The Node E2E passed 1/1 and proved main-process isolation, environment/stdin propagation, workspace writes, read-only denial, writable grants, hidden unmounted paths, post-check execution inside Alpine, health-check enforcement telemetry, server-info enforcement telemetry, cancellation and absence of leaked containers/custom sessions. Focused WSLC/policy tests pass 14/14; sandbox lifecycle tests pass 3/3 with the provisioned-storage live test skipped unless explicitly enabled; config/management tests pass 40/40. A direct TypeScript compile succeeds. The full Node suite reports 303 passed, 1 failed and 2 skipped; the single failure is the pre-existing `folderIsolation` process-control expectation, which conflicts with the separately passing session-id workspace-recovery contract and is outside the sandbox transport diff.

Remaining Node production gate: provide a supported provisioning path for the `wslc.session_storage` artifact (shared native provisioner, installer-managed storage, or another validated ownership mechanism). Until then, a default pure-Node installation with no provisioned storage correctly reports WSLC unavailable and fails closed instead of using the global WSLC session.

#### R7.6 - Automatic pure-Node WSLC session storage provisioning

Status: passed on the installed Windows/WSLC runtime. A default Node Agent no longer requires a pre-provisioned `wslc.session_storage` path when the Windows .NET Framework compiler is available.

The first provisioning experiment attempted to call `IWslcSessionManager.CreateSession` from C# loaded directly into PowerShell with `Add-Type`. Both PowerShell Core and Windows PowerShell reproducibly failed with `0x80070542` because the host process had already established COM security with an insufficient impersonation level before the script could call `CoInitializeSecurity`. Applying `CoSetProxyBlanket` with impersonation and static cloaking to the manager proxy did not change that result. The WSLC API itself was not the problem: the same fixed interop code compiled as a fresh .NET Framework process successfully created a new session storage directory, exited cleanly, and left the storage reusable by `wslc system session enter`.

The production Node provisioner therefore uses no bundled native addon or Rust sidecar. When no explicit storage override exists, it canonicalizes the selected workspace, hashes that identity with SHA-256, and derives `dataDir/sandbox/wslc/sessions/<32-hex-workspace-id>`. Existing storage must be a local directory containing a non-empty `storage.vhdx`; arbitrary or network-backed directories fail closed. An explicit `wslc.session_storage` override is still supported but must be an absolute local path and is subjected to the same validation.

On first use only, Node discovers the trusted system .NET Framework `csc.exe` under `%WINDIR%/Microsoft.NET/Framework64/v4.0.30319` or the 32-bit Framework fallback. It writes a constant product-owned C# source file to a random OS temporary directory, compiles a short-lived helper executable, and invokes it with only the generated session name and target storage path as structured argv. No command text, image name, environment value or workspace path is interpolated into the source. The helper initializes COM before any WSLC activation, uses the same validated `IWslcSessionManager` CLSID/interface/settings as the Rust provider (including NAT session transport and VirtioFS), creates the storage, releases the session/manager before `CoUninitialize`, and exits. Node then deletes the entire temporary compiler directory. Compiler discovery, compilation, helper startup, provisioning timeout, invalid storage and unsupported network paths all have structured fail-closed errors with `fallback_allowed=false`.

`server_info` can now report Node WSLC available without an explicit storage option when both the installed WSLC CLI and either a valid configured storage or a system `csc.exe` provisioner are present. Actual command preparation still validates or provisions the workspace-specific storage before opening a named session. The process-global default WSLC session remains completely unused.

Live validation removed `CTMCP_TEST_WSLC_SESSION_STORAGE` entirely. Starting from an empty temporary Agent data directory, the Node E2E automatically compiled the helper, created the VHD-backed storage, entered it under a product-owned named session, pulled/reused Alpine as needed, and passed the same main-process, stdin/environment, workspace-write, read-only/writable grant, hidden-path, post-check, health, server-telemetry and cancellation checks 1/1. The test additionally asserts the managed `storage.vhdx` exists before fixture cleanup. Focused WSLC/policy coverage is now 17/17 and sandbox/server-info lifecycle coverage 3/3 with the live test opt-in skipped by default; direct TypeScript compilation passes. After the live gate there are no product-created sessions, containers, temporary compiler directories, helper executables or research storage artifacts left behind.

Remaining parity concern: the current pure-Node transport intentionally takes an exclusive in-process lease on one storage while a named session is active. This is safe and fail-closed, but concurrent commands targeting the same workspace can currently receive `SANDBOX_WSLC_SESSION_BUSY` rather than sharing a coordinated session as the Rust backend does. The next Node-specific resilience gate is to replace that coarse lease with a shared named-session coordinator/mount-budget lifecycle or another bounded concurrency strategy without reintroducing the WSLC 15-mount leak.

#### R7.7 - Node same-storage FIFO admission

Status: passed on the installed WSLC runtime. The earlier fail-fast `SANDBOX_WSLC_SESSION_BUSY` behavior is replaced by cancellable FIFO admission, but active containers are deliberately not multiplexed into one named WSLC session.

The first implementation attempted to mirror the Rust coordinator more closely: multiple Node commands shared one product-owned named session, with an in-memory 15-mount generation budget and planned rotation after the active lease count reached zero. The pure budget model behaved as expected, but the live runtime rejected the concurrency model itself. Two simultaneous `wslc run` operations in the same custom session caused one container launch to exit with `E_UNEXPECTED` (`災難性的失敗`) on WSLC 2.9.4. This is a stronger compatibility boundary than the documented 15-mount limit, so the shared-active-session design was reverted rather than hidden behind retries.

The validated Node design now serializes each managed storage with an in-process FIFO. A command acquires the workspace storage token, opens a fresh product-owned named session, runs its container, terminates and fully tears down that session, and only then wakes the next waiter. Consequently each admitted command starts with a fresh session mount budget, so the 15-mount leak cannot accumulate across Node commands. Different workspace storage directories remain independently runnable; only commands targeting the same managed storage serialize. The global default WSLC session remains unused.

Storage admission is wired to the retained request `AbortSignal`. A request cancelled while queued is removed from the waiter list and returns `SANDBOX_WSLC_QUEUE_CANCELLED` before any named session or child process is created. A second cancellation check after wake-up closes the handoff race: an already-aborted waiter passes the storage token to the following request instead of consuming it. Session open failures and normal/forced cleanup release the token only after owner teardown, preventing the next waiter from attaching the same VHD while the previous session still owns it.

Live same-workspace concurrency testing starts two commands together. Both complete successfully with `sandbox_enforced=true` and WSLC output, while elapsed startup demonstrates that the second command waited for the first storage lease instead of failing with `SESSION_BUSY` or sharing the unstable active session. A second live case holds the FIFO with a sleeping command, queues another request that would write a leak marker, aborts that queued request, waits for the holder to finish, and verifies that the marker was never created. The full existing live WSLC chain still passes 1/1, including automatic storage provisioning, main-process isolation, post-checks, health/server telemetry and explicit container cancellation.

Validation after the FIFO change: TypeScript compilation passes; focused WSLC/policy coverage passes 17/17; sandbox/server-info lifecycle coverage passes 3 tests with the live test skipped by default; the opt-in FIFO/cancellation live regression passes 1/1. The complete Node suite contains 309 tests with 306 passed, 1 known unrelated `folderIsolation` contract failure and 2 skipped. Post-test inspection shows only pre-existing user WSLC CLI sessions, no product-created sessions, no containers and no temporary provisioner directories.

Remaining resilience gates are outside the in-process FIFO: two separate Node Agent processes can still target the same managed storage concurrently, and queued storage admission currently has no independent bounded wait/telemetry beyond request cancellation. Cross-process storage ownership should therefore be enforced with a recoverable OS-visible lock before treating same-data-directory multi-process operation as safe. Main command cleanup also releases the storage before its post-check acquires a new session, so another queued command may run between the main command and verification; this affects verification ordering/latency rather than the filesystem sandbox boundary, but should be considered if post-check atomicity becomes a product requirement.

#### R7.8 - Node cross-process WSLC storage admission

Status: passed on Windows. Multiple Node Agent processes that target the same managed WSLC session storage are now serialized by an OS-visible named mutex in addition to the existing in-process FIFO.

The storage identity is derived from the canonical storage path and hashed into a local Windows mutex name. A dedicated Windows PowerShell holder process acquires the mutex before `wslc system session enter`, retains it for the full named-session lifetime, and releases it only after WSLC owner teardown. The holder watches the parent Node process, so an Agent crash abandons/releases the mutex instead of permanently wedging the storage. Admission is bounded by the request timeout up to 120 seconds, is cancellable before session creation, and returns structured retryable timeout/cancellation errors without falling back to the process-global WSLC session.

The managed-storage availability probe now requires the cross-process lock host capability as well as WSLC and session-storage provisioning support. Main-command and post-check preparation pass their bounded command timeout into storage admission, preserving the existing timeout contract instead of introducing an unbounded lock wait.

The previously stale folder-isolation regression was also aligned with the established conversation routing contract: folder listings and operation IDs remain folder-scoped, while a conversation-scoped explicit `session_id` or `output_ref` can recover the original folder for process control without changing the conversation's selected folder.

Validation: TypeScript server build passes; dedicated cross-process lock tests pass 4/4, covering stable identity, bounded timeout, cancellation, and owner-crash recovery; focused WSLC/process lifecycle tests pass 22/22 with one live opt-in skip; the live WSLC end-to-end sandbox test passes with real container enforcement; folder-isolation tests pass 6/6; the complete Node Agent server suite passes 311/311 with two intentional live-environment skips. `git diff --check` passes.
## Primary references

- Microsoft Learn: Launch an AppContainer (`/windows/win32/secauthz/implementing-an-appcontainer`)
- Microsoft Learn: AppContainer isolation (`/windows/win32/secauthz/appcontainer-isolation`)
- Microsoft Learn: `UpdateProcThreadAttribute` / `PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES`
- Microsoft Learn: `CreateAppContainerProfile`
- Microsoft Learn: `DeriveAppContainerSidFromAppContainerName`
