use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use coding_tools_command_policy::resolved_command_timeout_ms as resolve_command_timeout_ms;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::sync::OwnedMutexGuard;

use crate::mcp::{classify_command_text, command_kind};
use crate::platform::wsl::invocation_for_path;
use crate::tools::context::ToolContext;
use crate::tools::process_start::{
    acquire_start_permission, is_loader_initialization_failure, loader_failure_retry_delay,
    spawn_with_control, spawn_with_permission, ProcessStartError, StartupDiagnostics,
    STARTUP_PROBE_WINDOW, STATUS_DLL_INIT_FAILED,
};
use crate::tools::redaction::arguments_reference_sensitive_source;
use crate::tools::session::{ExecSession, OutputMode, OutputOptions, DETACHED_SESSION_GRACE};
use crate::tools::workspace::{tool_ok, WorkspaceError};
use crate::tools::{ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, DEFAULT_COMMAND_TIMEOUT_MAX_MS};

#[derive(Clone, Debug)]
struct ExecSpec {
    display: String,
    program: String,
    args: Vec<String>,
    shell: String,
    env: Vec<(String, String)>,
    remove_env: Vec<String>,
}

#[derive(Clone, Debug)]
struct PowerShellRuntime {
    program: String,
    edition: &'static str,
}

#[derive(Clone, Debug)]
struct PowerShellDetection {
    selected: Option<PowerShellRuntime>,
    modern: Option<String>,
    legacy: Option<String>,
}

static POWERSHELL_DETECTION: OnceLock<PowerShellDetection> = OnceLock::new();

fn powershell_detection() -> &'static PowerShellDetection {
    POWERSHELL_DETECTION.get_or_init(|| {
        let modern = which::which("pwsh")
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let legacy = which::which("powershell")
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let selected = modern
            .as_ref()
            .map(|program| PowerShellRuntime {
                program: program.clone(),
                edition: "PowerShell Core",
            })
            .or_else(|| {
                legacy.as_ref().map(|program| PowerShellRuntime {
                    program: program.clone(),
                    edition: "Windows PowerShell",
                })
            });
        PowerShellDetection {
            selected,
            modern,
            legacy,
        }
    })
}

fn detected_powershell() -> Option<&'static PowerShellRuntime> {
    powershell_detection().selected.as_ref()
}

pub fn powershell_environment() -> Value {
    let detection = powershell_detection();
    json!({
        "selected": detected_powershell().map(|runtime| runtime.program.as_str()),
        "edition": detected_powershell().map(|runtime| runtime.edition),
        "pwsh_available": detection.modern.is_some(),
        "pwsh_path": detection.modern.as_deref(),
        "windows_powershell_available": detection.legacy.is_some(),
        "windows_powershell_path": detection.legacy.as_deref(),
        "selection_policy": "pwsh_then_windows_powershell",
        "output_encoding": "utf-8"
    })
}

fn preferred_powershell_program() -> Result<String, WorkspaceError> {
    detected_powershell()
        .map(|runtime| runtime.program.clone())
        .ok_or_else(|| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: "PowerShell 7 and Windows PowerShell are both unavailable.".into(),
            category: "runtime",
            retryable: false,
        })
}

fn powershell_utf8_script(script: &str) -> String {
    format!(
        "$__ctmcp_utf8=[System.Text.UTF8Encoding]::new($false); \
[Console]::InputEncoding = $__ctmcp_utf8; \
[Console]::OutputEncoding = $__ctmcp_utf8; \
$global:OutputEncoding = $__ctmcp_utf8; {script}"
    )
}

fn powershell_script(script: &str) -> String {
    powershell_utf8_script(script)
}

fn powershell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn normalize_powershell_args(args: &mut [String]) {
    if let Some(index) = args
        .iter()
        .position(|arg| arg.eq_ignore_ascii_case("-command") || arg.eq_ignore_ascii_case("-c"))
    {
        if let Some(script) = args.get_mut(index + 1) {
            *script = powershell_script(script);
        }
    }
}

fn is_powershell_name(value: &str) -> bool {
    Path::new(value)
        .file_stem()
        .and_then(|value| value.to_str())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("pwsh") || value.eq_ignore_ascii_case("powershell")
        })
}

#[derive(Clone, Debug)]
struct PostCheckSpec {
    name: String,
    exec: ExecSpec,
    expected_exit_code: i32,
    timeout: Duration,
    max_output_bytes: usize,
}

#[derive(Clone, Debug)]
struct ExecutionIdentity {
    operation_id: Option<String>,
    command_fingerprint: String,
    resource_lock_group: Option<String>,
    resource_lock_target: Option<String>,
}

const AUTO_DEDUPE_COMPLETED_GRACE: Duration = Duration::from_secs(30);

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_cargo_command(spec: &ExecSpec) -> bool {
    Path::new(&spec.program)
        .file_stem()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("cargo"))
        || spec.display.to_ascii_lowercase().contains("cargo ")
        || spec.display.to_ascii_lowercase().contains("tauri build")
}

fn resolved_command_timeout_ms(args: &Value, spec: &ExecSpec) -> u64 {
    resolve_command_timeout_ms(
        args.get("timeout_ms").and_then(Value::as_u64),
        command_kind(args),
        &spec.display,
        DEFAULT_COMMAND_TIMEOUT_MAX_MS,
        ABSOLUTE_COMMAND_TIMEOUT_MAX_MS,
    )
}

fn command_argument_value(args: &[String], name: &str) -> Option<String> {
    for (index, argument) in args.iter().enumerate() {
        if argument == name {
            return args.get(index + 1).cloned();
        }
        if let Some(value) = argument.strip_prefix(&format!("{name}=")) {
            return Some(value.to_string());
        }
    }
    None
}

fn normalized_lock_path(path: PathBuf) -> PathBuf {
    std::fs::canonicalize(&path).unwrap_or(path)
}

fn cargo_target_lock(spec: &ExecSpec, cwd: &Path) -> Option<(String, String)> {
    if !is_cargo_command(spec) {
        return None;
    }
    let env_target = spec
        .env
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("CARGO_TARGET_DIR"))
        .map(|(_, value)| value.clone());
    let target = command_argument_value(&spec.args, "--target-dir")
        .or(env_target)
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                cwd.join(path)
            }
        })
        .or_else(|| {
            command_argument_value(&spec.args, "--manifest-path").map(|manifest| {
                let manifest = PathBuf::from(manifest);
                let manifest = if manifest.is_absolute() {
                    manifest
                } else {
                    cwd.join(manifest)
                };
                manifest.parent().unwrap_or(cwd).join("target")
            })
        })
        .or_else(|| {
            let lower = spec.display.to_ascii_lowercase();
            let tauri_root = cwd.join("src-tauri");
            (lower.contains("tauri") && tauri_root.join("Cargo.toml").is_file())
                .then(|| tauri_root.join("target"))
        })
        .unwrap_or_else(|| cwd.join("target"));
    let target = normalized_lock_path(target);
    let display = target.to_string_lossy().into_owned();
    let digest = sha256_hex(display.as_bytes());
    Some((format!("cargo-target:{}", &digest[..24]), display))
}

fn execution_identity(
    args: &Value,
    spec: &ExecSpec,
    cwd: &Path,
    timeout_ms: u64,
    tty: bool,
    stdin_text: &str,
    post_checks: &[PostCheckSpec],
) -> ExecutionIdentity {
    let mut env = spec.env.clone();
    env.sort();
    let mut remove_env = spec.remove_env.clone();
    remove_env.sort();
    let post_checks = post_checks
        .iter()
        .map(|check| {
            json!({
                "name": check.name,
                "program": check.exec.program,
                "args": check.exec.args,
                "shell": check.exec.shell,
                "env": check.exec.env,
                "remove_env": check.exec.remove_env,
                "expected_exit_code": check.expected_exit_code,
                "timeout_ms": check.timeout.as_millis(),
                "max_output_bytes": check.max_output_bytes
            })
        })
        .collect::<Vec<_>>();
    let automatic_cargo_dedupe = is_cargo_command(spec)
        && matches!(
            command_kind(args),
            "cargo_test" | "cargo_check" | "build" | "format"
        );
    let explicit_operation_id = args
        .get("operation_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string);
    let deduplicate = explicit_operation_id.is_some()
        || args
            .get("deduplicate")
            .and_then(Value::as_bool)
            .unwrap_or(automatic_cargo_dedupe);
    let automatic_lock = cargo_target_lock(spec, cwd);
    let resource_lock_group = args
        .get("lock_group")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .or_else(|| automatic_lock.as_ref().map(|(group, _)| group.clone()));
    let resource_lock_target = automatic_lock.map(|(_, target)| target);
    let material = json!({
        "cwd": cwd.to_string_lossy(),
        "program": spec.program,
        "args": spec.args,
        "shell": spec.shell,
        "env": env,
        "remove_env": remove_env,
        "timeout_ms": timeout_ms,
        "tty": tty,
        "stdin_sha256": sha256_hex(stdin_text.as_bytes()),
        "post_checks": post_checks,
        "resource_lock_group": resource_lock_group
    });
    let command_fingerprint = sha256_hex(&serde_json::to_vec(&material).unwrap_or_default());
    let operation_id = explicit_operation_id
        .or_else(|| deduplicate.then(|| format!("auto:{}", &command_fingerprint[..32])));
    ExecutionIdentity {
        operation_id,
        command_fingerprint,
        resource_lock_group,
        resource_lock_target,
    }
}

struct RequestCancellationGuard {
    session: Option<std::sync::Arc<ExecSession>>,
}

impl RequestCancellationGuard {
    fn new(session: std::sync::Arc<ExecSession>) -> Self {
        Self {
            session: Some(session),
        }
    }

    fn disarm(&mut self) {
        self.session = None;
    }
}

impl Drop for RequestCancellationGuard {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        let detached_generation = session.mark_detached();
        crate::task_runtime::spawn(async move {
            tokio::time::sleep(DETACHED_SESSION_GRACE).await;
            if session.is_finalized() || !session.is_still_detached(detached_generation) {
                return;
            }
            session.mark_termination_reason("detached_timeout");
            if session.is_running().await {
                session.kill_and_wait().await;
            }
            session.mark_finalized();
        });
    }
}

pub fn exec_command(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    crate::task_runtime::block_on(exec_command_async(ctx, args))
}

pub async fn exec_command_async(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let workdir_raw = args
        .get("workdir")
        .or_else(|| args.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let workdir = ctx.workspace.resolve_existing(workdir_raw)?;
    if !workdir.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "workdir is not a directory",
        ));
    }
    let filesystem_scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace")
        .to_string();
    validate_child_process_scope(ctx, args)?;
    let runtime = ctx.runtime_config();
    let spec = resolve_exec_spec(args, &workdir.path, ctx.workspace.root(), &runtime.policy)?;
    let post_checks =
        resolve_post_checks(args, &workdir.path, ctx.workspace.root(), &runtime.policy)?;
    let output_options = OutputOptions::from_args(args, OutputMode::Tail);

    let legacy_native = !ctx.workspace.is_wsl()
        && args.get("program").is_none()
        && spec.shell == "none"
        && spec.env.is_empty()
        && spec.remove_env.is_empty()
        && post_checks.is_empty();
    if legacy_native {
        if let Some(result) = run_native_diagnostic(ctx, &spec.display, &workdir.path)? {
            let mut result = result;
            if let Some(object) = result.as_object_mut() {
                object.insert(
                    "filesystem_scope".into(),
                    Value::String(filesystem_scope.clone()),
                );
                object.insert("sandbox_enforced".into(), Value::Bool(false));
                object.insert(
                    "execution_boundary".into(),
                    Value::String("policy_only".into()),
                );
                object.insert("child_process".into(), Value::Bool(false));
                object.insert("transport_ok".into(), Value::Bool(true));
                object.insert("command_ok".into(), Value::Bool(true));
                object.insert("program".into(), json!(spec.program));
                object.insert("args".into(), json!(spec.args));
                object.insert("shell".into(), json!(spec.shell));
            }
            attach_session_capacity(ctx, &mut result);
            return Ok(tool_ok(result));
        }
    }
    let timeout_ms = resolved_command_timeout_ms(args, &spec);
    let yield_ms = args
        .get("yield_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(1000)
        .min(30_000);
    let tty = args.get("tty").and_then(Value::as_bool).unwrap_or(false);
    let stdin_text = args.get("stdin").and_then(Value::as_str).unwrap_or("");
    let sensitive_output = arguments_reference_sensitive_source(args);
    let identity = execution_identity(
        args,
        &spec,
        &workdir.path,
        timeout_ms,
        tty,
        stdin_text,
        &post_checks,
    );
    let operation_lock_started = Instant::now();
    let operation_guard: Option<OwnedMutexGuard<()>> =
        if let Some(operation_id) = identity.operation_id.as_deref() {
            let operation_lock_group = format!(
                "exec-operation:{}",
                &sha256_hex(operation_id.as_bytes())[..24]
            );
            Some(ctx.resource_lock(&operation_lock_group).lock_owned().await)
        } else {
            None
        };
    let operation_lock_wait_ms = operation_lock_started.elapsed().as_millis();

    if let Some(operation_id) = identity.operation_id.as_deref() {
        if let Some(session) = ctx.sessions.get_by_operation(operation_id) {
            let automatic_operation = operation_id.starts_with("auto:");
            let reuse_session = !automatic_operation
                || !session.is_finalized()
                || session.finalized_within(AUTO_DEDUPE_COMPLETED_GRACE);
            if automatic_operation && !reuse_session {
                ctx.sessions.remove(&session.session_id);
            } else {
                if session.command_fingerprint() != Some(identity.command_fingerprint.as_str()) {
                    return Err(WorkspaceError::ToolDetails {
                        code: "OPERATION_ID_CONFLICT",
                        message: "The operation_id is already associated with a different command."
                            .into(),
                        category: "validation",
                        retryable: false,
                        details: json!({
                            "operation_id": operation_id,
                            "requested_command_fingerprint": identity.command_fingerprint,
                            "existing_command_fingerprint": session.command_fingerprint(),
                            "existing_session_id": session.session_id,
                            "suggestion": "Reuse the original command arguments or choose a new operation_id."
                        }),
                    });
                }
                session.touch_attachment();
                session.refresh_status().await;
                let keep_session = !session.is_finalized();
                let mut out = merge_exec_result(
                    session.snapshot_with_options(output_options),
                    session.started_at,
                    &spec,
                    &workdir.path,
                    keep_session,
                    None,
                );
                if let Some(object) = out.as_object_mut() {
                    object.insert("deduplicated".into(), Value::Bool(true));
                    object.insert(
                        "attached_to_session_id".into(),
                        Value::String(session.session_id.clone()),
                    );
                    object.insert(
                        "operation_lock_wait_ms".into(),
                        json!(operation_lock_wait_ms),
                    );
                    object.insert("filesystem_scope".into(), Value::String(filesystem_scope));
                    object.insert("sandbox_enforced".into(), Value::Bool(false));
                    object.insert(
                        "execution_boundary".into(),
                        Value::String("policy_only".into()),
                    );
                    object.insert("child_process".into(), Value::Bool(true));
                }
                drop(operation_guard);
                attach_session_capacity(ctx, &mut out);
                return Ok(tool_ok(out));
            }
        }
    }

    let result = run_command(
        ctx,
        &spec,
        &workdir.path,
        Duration::from_millis(timeout_ms),
        Duration::from_millis(yield_ms),
        output_options,
        tty,
        stdin_text,
        post_checks,
        sensitive_output,
        identity,
        operation_lock_wait_ms,
        operation_guard,
    )
    .await;

    match result {
        Ok(mut out) => {
            if let Some(object) = out.as_object_mut() {
                object.insert("filesystem_scope".into(), Value::String(filesystem_scope));
                object.insert("sandbox_enforced".into(), Value::Bool(false));
                object.insert(
                    "execution_boundary".into(),
                    Value::String("policy_only".into()),
                );
                object.insert("child_process".into(), Value::Bool(true));
            }
            attach_session_capacity(ctx, &mut out);
            Ok(tool_ok(out))
        }
        Err(error) => match execution_failure_result(&error, &spec, &workdir.path) {
            Some(mut result) => {
                attach_session_capacity(ctx, &mut result);
                Ok(tool_ok(result))
            }
            None => Err(error),
        },
    }
}

fn attach_session_capacity(ctx: &ToolContext, value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "active_session_limit".into(),
            json!(ctx.sessions.active_session_limit()),
        );
        object.insert(
            "active_session_slots_available".into(),
            json!(ctx.sessions.active_slots_available()),
        );
    }
}

fn resolve_exec_spec(
    arguments: &Value,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<ExecSpec, WorkspaceError> {
    let wsl_workspace = crate::workspace::parse_wsl_path(workspace_root).is_some();
    let shell = arguments
        .get("shell")
        .and_then(Value::as_str)
        .unwrap_or("none")
        .to_ascii_lowercase();
    let env = arguments
        .get("env")
        .and_then(Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let remove_env = arguments
        .get("remove_env")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    if let Some(program) = arguments.get("program").and_then(Value::as_str) {
        if shell != "none" {
            return Err(WorkspaceError::invalid_argument(
                "program/args mode requires shell=none",
            ));
        }
        let mut args = arguments
            .get("args")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .map(|value| {
                        value.as_str().map(str::to_string).ok_or_else(|| {
                            WorkspaceError::invalid_argument("args entries must be strings")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let mut program = resolve_program(program, cwd, workspace_root, policy)?;
        if !wsl_workspace && is_powershell_name(&program) {
            program = preferred_powershell_program()?;
            normalize_powershell_args(&mut args);
        }
        let display = std::iter::once(program.clone())
            .chain(args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ");
        return finalize_exec_spec(
            ExecSpec {
                display,
                program,
                args,
                shell,
                env,
                remove_env,
            },
            cwd,
        );
    }

    let cmd = match (
        arguments.get("cmd").and_then(Value::as_str),
        arguments.get("script").and_then(Value::as_str),
    ) {
        (Some(_), Some(_)) => {
            return Err(WorkspaceError::invalid_argument(
                "Provide only one of cmd or script",
            ))
        }
        (Some(cmd), None) => cmd,
        (None, Some(script)) if shell != "none" => script,
        (None, Some(_)) => {
            return Err(WorkspaceError::invalid_argument(
                "script requires shell=powershell, cmd, or sh",
            ))
        }
        (None, None) => {
            return Err(WorkspaceError::invalid_argument(
                "cmd, script, or program is required",
            ))
        }
    };
    if shell == "none" {
        let (mut program, mut args) = parse_and_resolve(cmd, cwd, workspace_root, policy)?;
        if !wsl_workspace && is_powershell_name(&program) {
            program = preferred_powershell_program()?;
            normalize_powershell_args(&mut args);
        }
        return finalize_exec_spec(
            ExecSpec {
                display: cmd.to_string(),
                program,
                args,
                shell,
                env,
                remove_env,
            },
            cwd,
        );
    }

    let (shell_program, shell_args): (&str, Vec<String>) = match shell.as_str() {
        "cmd" => {
            if wsl_workspace {
                return Err(WorkspaceError::invalid_argument(
                    "shell=cmd is unavailable for WSL workspaces; use shell=sh",
                ));
            }
            #[cfg(windows)]
            {
                (
                    "cmd.exe",
                    vec!["/d".into(), "/s".into(), "/c".into(), cmd.into()],
                )
            }
            #[cfg(not(windows))]
            {
                return Err(WorkspaceError::invalid_argument(
                    "shell=cmd is only available on Windows",
                ));
            }
        }
        "powershell" => {
            if wsl_workspace {
                return Err(WorkspaceError::invalid_argument(
                    "shell=powershell is unavailable for WSL workspaces; use shell=sh",
                ));
            }
            let program = preferred_powershell_program()?;
            let args = vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                powershell_script(cmd),
            ];
            return finalize_exec_spec(
                ExecSpec {
                    display: cmd.to_string(),
                    program,
                    args,
                    shell,
                    env,
                    remove_env,
                },
                cwd,
            );
        }
        "sh" => ("sh", vec!["-c".into(), cmd.into()]),
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "shell must be none, cmd, powershell, or sh",
            ))
        }
    };
    let program = resolve_program(shell_program, cwd, workspace_root, policy)?;
    finalize_exec_spec(
        ExecSpec {
            display: cmd.to_string(),
            program,
            args: shell_args,
            shell,
            env,
            remove_env,
        },
        cwd,
    )
}

fn finalize_exec_spec(spec: ExecSpec, cwd: &Path) -> Result<ExecSpec, WorkspaceError> {
    validate_wsl_exec_paths(cwd, &spec)?;
    Ok(spec)
}

fn validate_wsl_exec_paths(cwd: &Path, spec: &ExecSpec) -> Result<(), WorkspaceError> {
    let Some(workspace_location) = crate::workspace::parse_wsl_path(cwd) else {
        return Ok(());
    };
    for (key, value) in &spec.env {
        if !valid_wsl_environment_key(key) || value.contains('\0') {
            return Err(WorkspaceError::ToolDetails {
                code: "WSL_ENVIRONMENT_INVALID",
                message: format!("env contains an invalid WSL environment entry: {key}"),
                category: "validation",
                retryable: false,
                details: json!({
                    "key": key,
                    "suggestion": "Use a non-empty environment name that does not start with '-' and contains neither '=' nor NUL."
                }),
            });
        }
    }
    for key in &spec.remove_env {
        if !valid_wsl_environment_key(key) {
            return Err(WorkspaceError::ToolDetails {
                code: "WSL_ENVIRONMENT_INVALID",
                message: format!("remove_env contains an invalid WSL environment name: {key}"),
                category: "validation",
                retryable: false,
                details: json!({
                    "key": key,
                    "suggestion": "Use a non-empty environment name that does not start with '-' and contains neither '=' nor NUL."
                }),
            });
        }
    }
    let values = std::iter::once(("program".to_string(), spec.program.as_str())).chain(
        spec.args
            .iter()
            .enumerate()
            .map(|(index, value)| (format!("args[{index}]"), value.as_str())),
    );
    for (position, value) in values {
        if let Some(path_location) = crate::workspace::parse_wsl_path(Path::new(value)) {
            if !path_location
                .distro
                .eq_ignore_ascii_case(&workspace_location.distro)
            {
                return Err(WorkspaceError::ToolDetails {
                    code: "WSL_CROSS_DISTRIBUTION_PATH",
                    message: format!(
                        "{position} references WSL distribution '{}' while the workspace runs in '{}'",
                        path_location.distro, workspace_location.distro
                    ),
                    category: "validation",
                    retryable: false,
                    details: json!({
                        "position": position,
                        "path": value,
                        "workspace_distro": workspace_location.distro,
                        "path_distro": path_location.distro,
                        "suggestion": "Copy the file into the workspace distribution or pass a Linux path available inside that distribution."
                    }),
                });
            }
            continue;
        }
        if looks_like_windows_host_path(value) {
            return Err(WorkspaceError::ToolDetails {
                code: "WSL_HOST_PATH_REQUIRES_TRANSLATION",
                message: format!(
                    "{position} uses a Windows host path that is not valid as a Linux command argument"
                ),
                category: "validation",
                retryable: true,
                details: json!({
                    "position": position,
                    "path": value,
                    "workspace_distro": workspace_location.distro,
                    "suggestion": "Use a workspace-relative path or the corresponding Linux mount path such as /mnt/c/..."
                }),
            });
        }
    }
    Ok(())
}

fn looks_like_windows_drive_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn looks_like_windows_host_path(value: &str) -> bool {
    looks_like_windows_drive_path(value) || value.starts_with(r"\\")
}

fn valid_wsl_environment_key(key: &str) -> bool {
    !key.is_empty() && !key.starts_with('-') && !key.contains(['=', '\0'])
}

fn resolve_post_checks(
    arguments: &Value,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<Vec<PostCheckSpec>, WorkspaceError> {
    let Some(checks) = arguments.get("post_checks") else {
        return Ok(Vec::new());
    };
    let checks = checks
        .as_array()
        .ok_or_else(|| WorkspaceError::invalid_argument("post_checks must be an array"))?;
    if checks.len() > 16 {
        return Err(WorkspaceError::invalid_argument(
            "post_checks supports at most 16 checks",
        ));
    }

    checks
        .iter()
        .enumerate()
        .map(|(index, check)| {
            let exec = resolve_exec_spec(check, cwd, workspace_root, policy)?;
            let name = check
                .get("name")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| format!("post-check-{}", index + 1));
            let expected_exit_code = check
                .get("expected_exit_code")
                .and_then(Value::as_i64)
                .unwrap_or(0)
                .clamp(i32::MIN as i64, i32::MAX as i64)
                as i32;
            let timeout = Duration::from_millis(
                check
                    .get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(30_000)
                    .clamp(1, ABSOLUTE_COMMAND_TIMEOUT_MAX_MS),
            );
            let max_output_bytes = check
                .get("max_output_bytes")
                .and_then(Value::as_u64)
                .unwrap_or(16_384)
                .clamp(1, 1_048_576) as usize;
            Ok(PostCheckSpec {
                name,
                exec,
                expected_exit_code,
                timeout,
                max_output_bytes,
            })
        })
        .collect()
}

fn validate_child_process_scope(_ctx: &ToolContext, args: &Value) -> Result<(), WorkspaceError> {
    let scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    match scope {
        "workspace" => Ok(()),
        "host" => Err(WorkspaceError::ToolDetails {
            code: "EXTERNAL_EXECUTION_NOT_ALLOWED",
            message: "exec_command 只允许在 Workspace 内执行，Workspace 外执行已禁用。".into(),
            category: "permission",
            retryable: false,
            details: json!({
                "stage": "policy",
                "filesystem_scope": "host",
                "sandbox_enforced": false,
                "recoverable": false,
                "suggestion": "将 filesystem_scope 设置为 workspace，并在当前 Workspace 内执行"
            }),
        }),
        _ => Err(WorkspaceError::invalid_argument(
            "filesystem_scope must be workspace",
        )),
    }
}

fn run_native_diagnostic(
    ctx: &ToolContext,
    cmd: &str,
    cwd: &Path,
) -> Result<Option<Value>, WorkspaceError> {
    let parts = shell_words::split(cmd)
        .map_err(|_| WorkspaceError::invalid_argument("Invalid command syntax"))?;
    if parts.is_empty() {
        return Ok(None);
    }

    let command = parts[0].to_ascii_lowercase();
    let stdout = match command.as_str() {
        "pwd" if parts.len() == 1 => Some(format!("{}\n", cwd.display())),
        "ls" | "dir" => Some(list_directory(ctx, cwd, &parts[1..])?),
        "which" if parts.len() == 2 => {
            let path = which::which(&parts[1]).map_err(|_| WorkspaceError::Tool {
                code: "COMMAND_NOT_FOUND",
                message: format!("Program not found on PATH: {}", parts[1]),
                category: "runtime",
                retryable: false,
            })?;
            Some(format!("{}\n", path.display()))
        }
        "echo" => Some(format!("{}\n", parts[1..].join(" "))),
        _ => None,
    };

    Ok(stdout.map(|stdout| {
        json!({
            "command": cmd,
            "resolved_cwd": cwd.display().to_string(),
            "status": "exited",
            "termination_reason": "exited",
            "recoverable": false,
            "suggestion": "命令已完成",
            "exit_code": 0,
            "stdout": stdout,
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "duration_ms": 0,
            "elapsed_ms": 0,
            "execution_mode": "native_builtin",
            "command_runner": "native_builtin",
            "warnings": ["native diagnostic without child process"]
        })
    }))
}

fn list_directory(
    ctx: &ToolContext,
    cwd: &Path,
    args: &[String],
) -> Result<String, WorkspaceError> {
    let target = match args {
        [] => cwd.to_path_buf(),
        [path] => ctx.workspace.resolve_existing(path)?.path,
        _ => {
            return Err(WorkspaceError::invalid_argument(
                "ls/dir accepts at most one directory path",
            ))
        }
    };
    if !target.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "ls/dir target is not a directory",
        ));
    }

    let mut entries = std::fs::read_dir(target)
        .map_err(|error| WorkspaceError::ToolDetails {
            code: "DIRECTORY_READ_FAILED",
            message: format!("Failed to read directory: {error}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "stage": "native_builtin",
                "reason": "directory_read_failed",
                "retryable": true
            }),
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    entries.sort_unstable();
    Ok(if entries.is_empty() {
        String::new()
    } else {
        format!("{}\n", entries.join("\n"))
    })
}

#[derive(Clone, Copy)]
enum CommandIoMode {
    Session,
    PostCheck,
}

fn prepared_command(spec: &ExecSpec, cwd: &Path, io_mode: CommandIoMode) -> Command {
    let wsl_invocation =
        invocation_for_path(cwd, &spec.program, &spec.args, &spec.env, &spec.remove_env);
    let using_wsl = wsl_invocation.is_some();
    let mut command = if let Some(invocation) = wsl_invocation {
        let mut command = Command::new(invocation.program);
        command.args(invocation.args);
        command
    } else {
        command_for_program(&spec.program, &spec.args)
    };
    if !using_wsl {
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        for key in &spec.remove_env {
            command.env_remove(key);
        }
        command.current_dir(platform_command_path(cwd));
    }
    command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    match io_mode {
        CommandIoMode::Session => {
            command.stdin(std::process::Stdio::piped());
        }
        CommandIoMode::PostCheck => {
            command
                .stdin(std::process::Stdio::null())
                .kill_on_drop(true);
        }
    }

    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

        command
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONLEGACYWINDOWSSTDIO", "0");
        command
            .as_std_mut()
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    command
}

fn process_start_error_json(error: &ProcessStartError) -> Value {
    match error {
        ProcessStartError::Spawn(source) => json!({
            "message": source.to_string(),
            "termination_reason": "spawn_failed",
            "recoverable": true
        }),
        ProcessStartError::LoaderInitialization {
            exit_code,
            diagnostics,
        } => json!({
            "message": error.to_string(),
            "termination_reason": "loader_initialization_failed",
            "recoverable": true,
            "process_exit_code": exit_code,
            "ntstatus": "0xc0000142",
            "startup": diagnostics.to_json()
        }),
    }
}

fn process_start_workspace_error(error: ProcessStartError) -> WorkspaceError {
    let details = process_start_error_json(&error);
    match error {
        ProcessStartError::Spawn(source) => WorkspaceError::ToolDetails {
            code: "COMMAND_SPAWN_FAILED",
            message: format!("Failed to start command: {source}"),
            category: "runtime",
            retryable: true,
            details: json!({
                "termination_reason": "spawn_failed",
                "recoverable": true,
                "suggestion": "检查命令路径、权限和运行时环境后重试"
            }),
        },
        ProcessStartError::LoaderInitialization { .. } => WorkspaceError::ToolDetails {
            code: "COMMAND_START_TRANSIENT_FAILURE",
            message: "Windows could not initialize the child process after controlled retries."
                .into(),
            category: "runtime",
            retryable: true,
            details,
        },
    }
}

fn session_process_exit_code(session: &ExecSession) -> Option<i32> {
    session
        .snapshot(1)
        .get("process_exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
}

#[allow(clippy::too_many_arguments)]
async fn run_command(
    ctx: &ToolContext,
    spec: &ExecSpec,
    cwd: &Path,
    limit: Duration,
    yield_time: Duration,
    output_options: OutputOptions,
    tty: bool,
    stdin_text: &str,
    post_checks: Vec<PostCheckSpec>,
    sensitive_output: bool,
    identity: ExecutionIdentity,
    operation_lock_wait_ms: u128,
    operation_guard: Option<OwnedMutexGuard<()>>,
) -> Result<Value, WorkspaceError> {
    let resource_lock_started = Instant::now();
    let resource_guard = if let Some(group) = identity.resource_lock_group.as_deref() {
        Some(ctx.resource_lock(group).lock_owned().await)
    } else {
        None
    };
    let resource_lock_wait_ms = resource_lock_started.elapsed().as_millis();
    let start = Instant::now();

    let mut startup_diagnostics = StartupDiagnostics::default();
    let (session, mut cancellation_guard, initial_cursor) = loop {
        let startup_permission = acquire_start_permission().await;
        let active_slot = ctx.sessions.acquire_active_slot().await?;
        let started = spawn_with_permission(startup_permission, || {
            prepared_command(spec, cwd, CommandIoMode::Session)
        })
        .map_err(process_start_workspace_error)?;
        startup_diagnostics.absorb(&started.diagnostics);
        let startup_guard = started.startup_guard;
        let session = ctx.sessions.insert(
            ExecSession::new_with_mode_and_checks(started.child, tty, !post_checks.is_empty())
                .with_active_slot(active_slot)
                .with_sensitive_output(sensitive_output)
                .with_telemetry(&ctx.profile_id, classify_command_text(&spec.display))
                .with_execution_identity(
                    identity.operation_id.clone(),
                    identity.command_fingerprint.clone(),
                    identity.resource_lock_group.clone(),
                    identity.resource_lock_target.clone(),
                    operation_lock_wait_ms,
                    resource_lock_wait_ms,
                ),
        );
        let mut cancellation_guard = RequestCancellationGuard::new(session.clone());
        let initial_cursor = session.latest_cursor();
        session.spawn_readers().await;
        session.spawn_exit_waiter();

        let exited_during_probe =
            tokio::time::timeout(STARTUP_PROBE_WINDOW, session.wait_until_exited())
                .await
                .is_ok();
        drop(startup_guard);
        let loader_failed = exited_during_probe
            && is_loader_initialization_failure(session_process_exit_code(&session));
        if !loader_failed {
            break (session, cancellation_guard, initial_cursor);
        }

        cancellation_guard.disarm();
        session.wait_for_readers().await;
        session.mark_finalized();
        ctx.sessions.remove(&session.session_id);

        let retry_index = startup_diagnostics.attempts - 1;
        let Some(delay) = loader_failure_retry_delay(retry_index).await else {
            return Err(process_start_workspace_error(
                ProcessStartError::LoaderInitialization {
                    exit_code: STATUS_DLL_INIT_FAILED,
                    diagnostics: startup_diagnostics,
                },
            ));
        };
        startup_diagnostics
            .retry_delays_ms
            .push(delay.as_millis() as u64);
        eprintln!(
            "child process loader initialization failed (0xc0000142); retrying in {} ms",
            delay.as_millis()
        );
        tokio::time::sleep(delay).await;
    };

    let deadline = start + limit;
    spawn_lifecycle_monitor(
        session.clone(),
        deadline,
        post_checks,
        cwd.to_path_buf(),
        resource_guard,
    );
    drop(operation_guard);

    if !tty && !stdin_text.is_empty() {
        let mut stdin_guard = session.stdin.lock().await;
        if let Some(stdin) = stdin_guard.as_mut() {
            use tokio::io::AsyncWriteExt;
            if !stdin_text.is_empty() {
                stdin
                    .write_all(stdin_text.as_bytes())
                    .await
                    .map_err(|_| WorkspaceError::Tool {
                        code: "SESSION_CLOSED",
                        message: "Failed to write stdin.".into(),
                        category: "runtime",
                        retryable: false,
                    })?;
            }
            let _ = stdin.shutdown().await;
        }
        *stdin_guard = None;
        session.mark_stdin_closed();
    }

    if yield_time.is_zero() || tty {
        let snapshot = session.snapshot_with_options(output_options);
        cancellation_guard.disarm();
        return Ok(merge_exec_result(
            snapshot,
            start,
            spec,
            cwd,
            true,
            Some(&startup_diagnostics),
        ));
    }

    let changed = session
        .wait_for_change(initial_cursor, yield_time, "output_or_exit")
        .await;
    session.refresh_status().await;
    if changed && !session.has_exited() {
        let remaining_yield = yield_time.saturating_sub(start.elapsed());
        let quick_exit_grace = remaining_yield.min(Duration::from_millis(500));
        if !quick_exit_grace.is_zero() {
            let _ = session
                .wait_for_change(session.latest_cursor(), quick_exit_grace, "exit")
                .await;
            session.refresh_status().await;
        }
    }
    if session.has_exited() && !session.is_finalized() {
        let remaining_yield = yield_time.saturating_sub(start.elapsed());
        if !remaining_yield.is_zero() {
            let _ = session
                .wait_for_change(session.latest_cursor(), remaining_yield, "finalized")
                .await;
        }
    }
    if session.is_finalized() {
        let snapshot = session.snapshot_with_options(output_options);
        cancellation_guard.disarm();
        Ok(merge_exec_result(
            snapshot,
            start,
            spec,
            cwd,
            false,
            Some(&startup_diagnostics),
        ))
    } else {
        let snapshot = session.snapshot_with_options(output_options);
        cancellation_guard.disarm();
        Ok(merge_exec_result(
            snapshot,
            start,
            spec,
            cwd,
            true,
            Some(&startup_diagnostics),
        ))
    }
}

fn spawn_lifecycle_monitor(
    session: std::sync::Arc<ExecSession>,
    deadline: Instant,
    post_checks: Vec<PostCheckSpec>,
    cwd: std::path::PathBuf,
    resource_guard: Option<OwnedMutexGuard<()>>,
) {
    tokio::spawn(async move {
        let _resource_guard = resource_guard;
        tokio::select! {
            _ = session.wait_until_exited() => {}
            _ = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                if !session.has_exited() {
                session.mark_termination_reason("process_timeout");
                session.kill_and_wait().await;
                }
            }
        }

        session.wait_for_readers().await;
        if post_checks.is_empty() {
            session.mark_finalized();
            return;
        }

        let main = session.snapshot_with_options(OutputOptions {
            mode: OutputMode::None,
            cursor: session.latest_cursor(),
            max_output_bytes: 1,
            tail_lines: 1,
        });
        if main.get("execution_ok").and_then(Value::as_bool) != Some(true) {
            session.complete_post_checks(json!({
                "ok": false,
                "configured": post_checks.len(),
                "executed": 0,
                "skipped": true,
                "reason": "main_command_failed",
                "results": []
            }));
            return;
        }

        let configured = post_checks.len();
        let max_concurrency = configured.min(4).max(1);
        let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrency));
        let mut tasks = tokio::task::JoinSet::new();
        for (index, check) in post_checks.into_iter().enumerate() {
            let semaphore = semaphore.clone();
            let cwd = cwd.clone();
            tasks.spawn(async move {
                let _permit = semaphore.acquire_owned().await.ok();
                (index, run_post_check(&check, &cwd).await)
            });
        }

        let mut indexed_results = Vec::with_capacity(configured);
        while let Some(joined) = tasks.join_next().await {
            match joined {
                Ok(result) => indexed_results.push(result),
                Err(error) => indexed_results.push((
                    usize::MAX,
                    json!({
                        "name": "post-check-worker",
                        "passed": false,
                        "timed_out": false,
                        "stderr": error.to_string(),
                        "duration_ms": 0
                    }),
                )),
            }
        }
        indexed_results.sort_by_key(|(index, _)| *index);
        let results = indexed_results
            .into_iter()
            .map(|(_, result)| result)
            .collect::<Vec<_>>();
        let all_ok = results
            .iter()
            .all(|result| result.get("passed").and_then(Value::as_bool) == Some(true));
        session.complete_post_checks(json!({
            "ok": all_ok,
            "configured": configured,
            "executed": results.len(),
            "skipped": false,
            "execution_mode": "parallel",
            "max_concurrency": max_concurrency,
            "results": results
        }));
    });
}

async fn run_post_check(check: &PostCheckSpec, cwd: &Path) -> Value {
    let start = Instant::now();
    let output = tokio::time::timeout(check.timeout, async {
        match spawn_with_control(|| prepared_command(&check.exec, cwd, CommandIoMode::PostCheck))
            .await
        {
            Ok(started) => {
                let diagnostics = started.diagnostics;
                match started.child.wait_with_output().await {
                    Ok(output) => Ok((output, diagnostics)),
                    Err(error) => Err(json!({
                        "message": error.to_string(),
                        "startup": diagnostics.to_json()
                    })),
                }
            }
            Err(error) => Err(process_start_error_json(&error)),
        }
    })
    .await;
    match output {
        Ok(Ok((output, diagnostics))) => {
            let process_exit_code = output.status.code();
            let passed = process_exit_code == Some(check.expected_exit_code);
            let (stdout, stdout_truncated) = bounded_output(&output.stdout, check.max_output_bytes);
            let (stderr, stderr_truncated) = bounded_output(&output.stderr, check.max_output_bytes);
            json!({
                "name": check.name,
                "command": check.exec.display,
                "process_exit_code": process_exit_code,
                "expected_exit_code": check.expected_exit_code,
                "passed": passed,
                "timed_out": false,
                "stdout": stdout,
                "stderr": stderr,
                "stdout_truncated": stdout_truncated,
                "stderr_truncated": stderr_truncated,
                "startup": diagnostics.to_json(),
                "duration_ms": start.elapsed().as_millis()
            })
        }
        Ok(Err(error)) => json!({
            "name": check.name,
            "command": check.exec.display,
            "process_exit_code": null,
            "expected_exit_code": check.expected_exit_code,
            "passed": false,
            "timed_out": false,
            "stdout": "",
            "stderr": error["message"].as_str().unwrap_or("post-check process failed"),
            "stdout_truncated": false,
            "stderr_truncated": false,
            "startup": error["startup"].clone(),
            "startup_error": error,
            "duration_ms": start.elapsed().as_millis()
        }),
        Err(_) => json!({
            "name": check.name,
            "command": check.exec.display,
            "process_exit_code": null,
            "expected_exit_code": check.expected_exit_code,
            "passed": false,
            "timed_out": true,
            "stdout": "",
            "stderr": "post-check timed out",
            "stdout_truncated": false,
            "stderr_truncated": false,
            "duration_ms": start.elapsed().as_millis()
        }),
    }
}

fn bounded_output(bytes: &[u8], max_output_bytes: usize) -> (String, bool) {
    let truncated = bytes.len() > max_output_bytes;
    let take = bytes.len().min(max_output_bytes);
    (
        String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(take)..]).into_owned(),
        truncated,
    )
}

pub fn exec_health_check(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let start = Instant::now();
    let cwd = ctx.workspace.root().to_path_buf();
    let probe_args = if ctx.workspace.is_wsl() {
        json!({
            "script": "printf exec-health; printf exec-health-stderr >&2",
            "shell": "sh",
            "confirm": true
        })
    } else {
        #[cfg(windows)]
        {
            json!({"cmd": r#"cmd.exe /d /c "echo exec-health && echo exec-health-stderr 1>&2""#})
        }
        #[cfg(not(windows))]
        {
            json!({"cmd": r#"sh -c "printf exec-health; printf exec-health-stderr >&2""#})
        }
    };
    let runtime = ctx.runtime_config();
    let spec = resolve_exec_spec(&probe_args, &cwd, ctx.workspace.root(), &runtime.policy)?;
    let identity = execution_identity(&probe_args, &spec, &cwd, 5000, false, "", &[]);
    let result = crate::task_runtime::block_on(run_command(
        ctx,
        &spec,
        &cwd,
        Duration::from_secs(5),
        Duration::from_secs(5),
        OutputOptions::tail(16_384),
        false,
        "",
        Vec::new(),
        false,
        identity,
        0,
        None,
    ));

    let mut response = json!({
        "worker": {"alive": true},
        "session_create": false,
        "command_run": false,
        "stdout_capture": false,
        "stderr_capture": false,
        "duration_ms": start.elapsed().as_millis(),
        "next_actions": []
    });

    match result {
        Ok(snapshot) => {
            let session_created = snapshot.get("session_id").is_some();
            let command_run = snapshot.get("exit_code").and_then(Value::as_i64) == Some(0);
            let stdout_capture = snapshot
                .get("stdout")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("exec-health"));
            let stderr_capture = snapshot
                .get("stderr")
                .and_then(Value::as_str)
                .is_some_and(|value| value.contains("exec-health-stderr"));
            let healthy = session_created && command_run && stdout_capture && stderr_capture;
            response["session_create"] = Value::Bool(session_created);
            response["command_run"] = Value::Bool(command_run);
            response["stdout_capture"] = Value::Bool(stdout_capture);
            response["stderr_capture"] = Value::Bool(stderr_capture);
            response["status"] = Value::String(if healthy { "success" } else { "error" }.into());
            response["summary"] = Value::String(if healthy {
                "exec worker、session、命令执行和 stdout/stderr 捕获均正常".into()
            } else {
                "exec health check 未通过，请查看 probe 结果".into()
            });
            response["probe"] = snapshot;
            if !healthy {
                response["next_actions"] = json!(["检查 exec worker 日志", "重启运行时"]);
            }
        }
        Err(error) => {
            response["status"] = Value::String("error".into());
            response["summary"] = Value::String("exec session 创建或探针执行失败".into());
            response["error"] = error.to_error_value();
            response["next_actions"] = json!(["检查 exec worker 日志", "重启运行时"]);
        }
    }
    response["duration_ms"] = json!(start.elapsed().as_millis());
    Ok(tool_ok(response))
}

fn execution_failure_result(error: &WorkspaceError, spec: &ExecSpec, cwd: &Path) -> Option<Value> {
    let code = match &error {
        WorkspaceError::Tool { code, .. } | WorkspaceError::ToolDetails { code, .. } => *code,
    };
    if !matches!(
        code,
        "COMMAND_REJECTED" | "COMMAND_SPAWN_FAILED" | "TIMEOUT"
    ) {
        return None;
    }

    let error_value = error.to_error_value();
    let details = error_value
        .get("details")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut result = details.get("session").cloned().unwrap_or_else(|| {
        json!({
            "status": "spawn_failed",
            "termination_reason": "spawn_failed",
            "recoverable": error_value["retryable"].as_bool().unwrap_or(false),
            "process_exit_code": Value::Null,
            "exit_code": Value::Null,
            "request_timed_out": false,
            "process_timed_out": false,
            "process_still_running": false,
            "stdout": "",
            "stderr": "",
            "stdout_truncated": false,
            "stderr_truncated": false
        })
    });
    if let Some(object) = result.as_object_mut() {
        object.insert("command".into(), json!(spec.display));
        object.insert("program".into(), json!(spec.program));
        object.insert("args".into(), json!(spec.args));
        object.insert("shell".into(), json!(spec.shell));
        object.insert(
            "environment_keys".into(),
            json!(spec.env.iter().map(|(key, _)| key).collect::<Vec<_>>()),
        );
        object.insert("removed_environment_keys".into(), json!(spec.remove_env));
        object.insert("resolved_cwd".into(), json!(cwd.display().to_string()));
        object.insert("execution_mode".into(), json!("direct"));
        object.insert("filesystem_scope".into(), json!("workspace"));
        object.insert("sandbox_enforced".into(), Value::Bool(false));
        object.insert("execution_boundary".into(), json!("policy_only"));
        object.insert("child_process".into(), Value::Bool(true));
        object.insert("transport_ok".into(), Value::Bool(true));
        object.insert("command_ok".into(), Value::Bool(false));
        object.insert("error".into(), error_value);
        if code == "TIMEOUT" {
            object.insert("status".into(), json!("timed_out"));
            object.insert("termination_reason".into(), json!("process_timeout"));
            object.insert("process_timed_out".into(), Value::Bool(true));
        } else {
            object.insert("status".into(), json!("spawn_failed"));
            object.insert("termination_reason".into(), json!("spawn_failed"));
        }
    }
    Some(result)
}

fn merge_exec_result(
    mut snapshot: Value,
    start: Instant,
    spec: &ExecSpec,
    cwd: &Path,
    keep_session: bool,
    startup_diagnostics: Option<&StartupDiagnostics>,
) -> Value {
    if let Some(obj) = snapshot.as_object_mut() {
        let duration_ms = start.elapsed().as_millis();
        obj.insert("command".into(), json!(spec.display));
        obj.insert("program".into(), json!(spec.program));
        obj.insert("args".into(), json!(spec.args));
        obj.insert("shell".into(), json!(spec.shell));
        obj.insert(
            "environment_keys".into(),
            json!(spec.env.iter().map(|(key, _)| key).collect::<Vec<_>>()),
        );
        obj.insert("removed_environment_keys".into(), json!(spec.remove_env));
        let process_tree_contained = obj
            .get("process_tree_contained")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        obj.insert(
            "process_tree_control".into(),
            json!(if process_tree_contained {
                "windows_job_object"
            } else if cfg!(windows) {
                "new_process_group"
            } else {
                "child_process"
            }),
        );
        obj.insert("resolved_cwd".into(), json!(cwd.display().to_string()));
        obj.insert("duration_ms".into(), json!(duration_ms));
        obj.insert("elapsed_ms".into(), json!(duration_ms));
        obj.insert("transport_ok".into(), Value::Bool(true));
        obj.insert("execution_mode".into(), json!("direct"));
        obj.insert("warnings".into(), json!(Vec::<&str>::new()));
        if let Some(diagnostics) = startup_diagnostics {
            obj.insert("startup".into(), diagnostics.to_json());
        }
        if keep_session {
            let session_id = obj
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string);
            if let Some(session_id) = session_id {
                let cursor = obj.get("next_cursor").and_then(Value::as_u64).unwrap_or(0);
                obj.insert(
                    "next_actions".into(),
                    json!([{
                        "tool": "wait_command",
                        "arguments": {
                            "session_id": session_id,
                            "cursor": cursor,
                            "timeout_ms": 120000,
                            "until": "finalized",
                            "output_mode": "delta"
                        }
                    }]),
                );
                obj.insert(
                    "suggestion".into(),
                    json!("Call wait_command with next_actions[0].arguments to continue without polling duplicate output"),
                );
            }
        }
    }
    snapshot
}

fn parse_and_resolve(
    cmd: &str,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<(String, Vec<String>), WorkspaceError> {
    let parts = shell_words::split(cmd)
        .map_err(|_| WorkspaceError::invalid_argument("Invalid command syntax"))?;
    if parts.is_empty() {
        return Err(WorkspaceError::invalid_argument("Empty command"));
    }

    let program = resolve_program(&parts[0], cwd, workspace_root, policy)?;
    Ok((program, parts[1..].to_vec()))
}

fn resolve_program(
    raw: &str,
    cwd: &Path,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<String, WorkspaceError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(WorkspaceError::invalid_argument("Empty program"));
    }

    let wsl_workspace = crate::workspace::parse_wsl_path(workspace_root).is_some();
    if wsl_workspace && trimmed.starts_with('/') {
        return resolve_wsl_absolute_program(trimmed, workspace_root, policy);
    }
    let explicit_path = trimmed.contains(['/', '\\']);
    let candidate = if Path::new(trimmed).is_absolute() {
        Path::new(trimmed).to_path_buf()
    } else {
        cwd.join(trimmed)
    };
    if candidate.is_file() {
        let resolved = candidate.canonicalize().map_err(|_| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        })?;
        let canonical_workspace =
            workspace_root
                .canonicalize()
                .map_err(|_| WorkspaceError::Tool {
                    code: "COMMAND_REJECTED",
                    message: "Workspace root is unavailable".into(),
                    category: "runtime",
                    retryable: true,
                })?;
        if !resolved.starts_with(&canonical_workspace) {
            return Err(WorkspaceError::Tool {
                code: "EXECUTABLE_OUTSIDE_WORKSPACE",
                message: format!("Workspace 外可执行文件被拒绝: {trimmed}"),
                category: "security",
                retryable: false,
            });
        }
        if workspace_local_program_allowed(&resolved, policy) {
            if wsl_workspace {
                return Ok(trimmed.replace('\\', "/"));
            }
            return Ok(resolved.to_string_lossy().into_owned());
        }
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Workspace 本地入口未获允许: {trimmed}"),
            category: "policy",
            retryable: false,
        });
    }

    if explicit_path {
        return Err(WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found: {trimmed}"),
            category: "runtime",
            retryable: false,
        });
    }

    if wsl_workspace {
        // The policy layer already validates the inner executable name against
        // allowed_commands. Availability is determined by the target distro,
        // not by the Windows PATH used by this desktop process.
        return Ok(trimmed.to_string());
    }

    which::which(trimmed)
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| WorkspaceError::Tool {
            code: "COMMAND_REJECTED",
            message: format!("Program not found on PATH: {trimmed}"),
            category: "runtime",
            retryable: false,
        })
}

fn resolve_wsl_absolute_program(
    raw: &str,
    workspace_root: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> Result<String, WorkspaceError> {
    let normalized =
        normalize_wsl_absolute_program_path(raw).ok_or_else(|| WorkspaceError::Tool {
            code: "EXECUTABLE_OUTSIDE_WORKSPACE",
            message: format!("Workspace 外可执行文件被拒绝: {raw}"),
            category: "security",
            retryable: false,
        })?;
    let location = crate::workspace::parse_wsl_path(workspace_root)
        .ok_or_else(|| WorkspaceError::invalid_argument("WSL workspace location is unavailable"))?;
    let host_candidate = PathBuf::from(crate::workspace::wsl_unc_path(
        &location.distro,
        &normalized,
    ));

    if host_candidate.is_file() {
        if let (Ok(resolved), Ok(canonical_workspace)) =
            (host_candidate.canonicalize(), workspace_root.canonicalize())
        {
            if resolved.starts_with(&canonical_workspace) {
                if workspace_local_program_allowed(&resolved, policy) {
                    return Ok(normalized);
                }
                return Err(WorkspaceError::Tool {
                    code: "COMMAND_REJECTED",
                    message: format!("Workspace 本地入口未获允许: {raw}"),
                    category: "policy",
                    retryable: false,
                });
            }
        }
    }

    let base_name = normalized.rsplit('/').next().unwrap_or(&normalized);
    let stem = base_name.strip_suffix(".exe").unwrap_or(base_name);
    if policy.allowed_commands.contains(stem) && is_trusted_wsl_system_program(&normalized) {
        return Ok(normalized);
    }

    Err(WorkspaceError::Tool {
        code: "EXECUTABLE_OUTSIDE_WORKSPACE",
        message: format!("Workspace 外可执行文件被拒绝: {raw}"),
        category: "security",
        retryable: false,
    })
}

fn normalize_wsl_absolute_program_path(raw: &str) -> Option<String> {
    if !raw.starts_with('/') || raw.contains(['\\', '\0']) || raw.chars().any(char::is_control) {
        return None;
    }
    let mut segments = Vec::new();
    for segment in raw.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            value => segments.push(value),
        }
    }
    Some(if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    })
}

fn is_trusted_wsl_system_program(path: &str) -> bool {
    let Some((parent, name)) = path.rsplit_once('/') else {
        return false;
    };
    !name.is_empty()
        && matches!(
            parent,
            "/bin"
                | "/sbin"
                | "/usr/bin"
                | "/usr/sbin"
                | "/usr/local/bin"
                | "/usr/local/sbin"
                | "/snap/bin"
        )
}

fn workspace_local_program_allowed(
    resolved: &Path,
    policy: &crate::tools::policy::PolicySettings,
) -> bool {
    let extension = resolved
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| format!(".{}", value.to_ascii_lowercase()))
        .unwrap_or_default();
    policy.workspace_local_entries
        && (extension.is_empty() || policy.workspace_script_extensions.contains(&extension))
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::tools::context::ToolContext;
    use crate::tools::dispatch::call_tool;
    use serde_json::json;
    use tempfile::tempdir;

    fn assert_failure_result(error: WorkspaceError, expected_code: &str) {
        // Kept near timeout inference tests so failures remain easy to diagnose.
        let spec = ExecSpec {
            display: "missing-command".into(),
            program: "missing-command".into(),
            args: Vec::new(),
            shell: "none".into(),
            env: Vec::new(),
            remove_env: Vec::new(),
        };
        let result = execution_failure_result(&error, &spec, Path::new("C:/workspace"))
            .expect("应转换为统一执行结果");
        assert_eq!(result["transport_ok"], true);
        assert_eq!(result["command_ok"], false);
        assert_eq!(result["status"], "spawn_failed");
        assert_eq!(result["error"]["code"], expected_code);
    }

    #[cfg(windows)]
    #[test]
    fn wsl_workspace_wraps_the_inner_command_without_windows_path_resolution() {
        let root = Path::new(r"\\?\UNC\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject");
        let request = json!({"cmd": "cargo test"});
        let spec = resolve_exec_spec(
            &request,
            root,
            root,
            &crate::tools::policy::PolicySettings::default(),
        )
        .expect("WSL exec spec");
        let command = prepared_command(&spec, root, CommandIoMode::PostCheck);
        let command = command.as_std();

        assert_eq!(command.get_program(), "wsl.exe");
        assert_eq!(
            command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec![
                "--distribution",
                "Ubuntu-24.04",
                "--cd",
                "/opt/src/SampleProject",
                "--exec",
                "cargo",
                "test",
            ]
        );

        let absolute = resolve_exec_spec(
            &json!({"program": "/usr/bin/cargo", "args": ["check"]}),
            root,
            root,
            &crate::tools::policy::PolicySettings::default(),
        )
        .expect("allowlisted absolute WSL program");
        assert_eq!(absolute.program, "/usr/bin/cargo");

        let spoofed = resolve_exec_spec(
            &json!({"program": "/tmp/cargo", "args": ["check"]}),
            root,
            root,
            &crate::tools::policy::PolicySettings::default(),
        )
        .expect_err("allowlisted basename outside trusted system directories must be rejected")
        .to_error_value();
        assert_eq!(spoofed["code"], "EXECUTABLE_OUTSIDE_WORKSPACE");

        let traversed = resolve_exec_spec(
            &json!({"program": "/usr/bin/../../tmp/cargo", "args": ["check"]}),
            root,
            root,
            &crate::tools::policy::PolicySettings::default(),
        )
        .expect_err("path traversal must not disguise an untrusted executable")
        .to_error_value();
        assert_eq!(traversed["code"], "EXECUTABLE_OUTSIDE_WORKSPACE");
    }

    #[test]
    fn wsl_absolute_program_normalization_is_lexical_and_bounded() {
        assert_eq!(
            normalize_wsl_absolute_program_path("/usr/bin/../local/bin/cargo"),
            Some("/usr/local/bin/cargo".into())
        );
        assert_eq!(
            normalize_wsl_absolute_program_path("/usr//bin/./cargo"),
            Some("/usr/bin/cargo".into())
        );
        assert_eq!(
            normalize_wsl_absolute_program_path("/../../tmp/cargo"),
            None
        );
        assert!(is_trusted_wsl_system_program("/usr/bin/cargo"));
        assert!(is_trusted_wsl_system_program("/snap/bin/prettier"));
        assert!(!is_trusted_wsl_system_program("/tmp/cargo"));
        assert!(!is_trusted_wsl_system_program("/home/dev/bin/cargo"));
    }

    #[cfg(windows)]
    #[test]
    fn wsl_workspace_rejects_paths_unavailable_to_the_target_distribution() {
        let root = Path::new(r"\\?\UNC\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject");
        let policy = crate::tools::policy::PolicySettings::default();

        let cross_distro = resolve_exec_spec(
            &json!({
                "program": "cargo",
                "args": [r"\\wsl.localhost\Debian\tmp\Cargo.toml"]
            }),
            root,
            root,
            &policy,
        )
        .expect_err("cross-distribution path must be rejected")
        .to_error_value();
        assert_eq!(cross_distro["code"], "WSL_CROSS_DISTRIBUTION_PATH");
        assert_eq!(cross_distro["details"]["workspace_distro"], "Ubuntu-24.04");
        assert_eq!(cross_distro["details"]["path_distro"], "Debian");

        let host_path = resolve_exec_spec(
            &json!({"program": "cargo", "args": [r"C:\src\Cargo.toml"]}),
            root,
            root,
            &policy,
        )
        .expect_err("Windows host path must be translated")
        .to_error_value();
        assert_eq!(host_path["code"], "WSL_HOST_PATH_REQUIRES_TRANSLATION");
        assert_eq!(host_path["details"]["position"], "args[0]");

        let unc_host_path = resolve_exec_spec(
            &json!({"program": "cargo", "args": [r"\\server\share\Cargo.toml"]}),
            root,
            root,
            &policy,
        )
        .expect_err("Windows UNC host path must be rejected")
        .to_error_value();
        assert_eq!(unc_host_path["code"], "WSL_HOST_PATH_REQUIRES_TRANSLATION");

        let invalid_env = resolve_exec_spec(
            &json!({"program": "cargo", "args": ["check"], "env": {"--help": "1"}}),
            root,
            root,
            &policy,
        )
        .expect_err("option-like WSL environment names must be rejected")
        .to_error_value();
        assert_eq!(invalid_env["code"], "WSL_ENVIRONMENT_INVALID");

        let invalid_removed_env = resolve_exec_spec(
            &json!({"program": "cargo", "args": ["check"], "remove_env": ["A=B"]}),
            root,
            root,
            &policy,
        )
        .expect_err("invalid removed WSL environment names must be rejected")
        .to_error_value();
        assert_eq!(invalid_removed_env["code"], "WSL_ENVIRONMENT_INVALID");
    }

    #[test]
    fn 程序不存在时返回统一执行结果() {
        assert_failure_result(
            WorkspaceError::Tool {
                code: "COMMAND_REJECTED",
                message: "Program not found on PATH: missing-command".into(),
                category: "runtime",
                retryable: false,
            },
            "COMMAND_REJECTED",
        );
    }

    #[test]
    fn 启动失败时返回统一执行结果() {
        assert_failure_result(
            WorkspaceError::ToolDetails {
                code: "COMMAND_SPAWN_FAILED",
                message: "Failed to start command".into(),
                category: "runtime",
                retryable: true,
                details: json!({"recoverable": true}),
            },
            "COMMAND_SPAWN_FAILED",
        );
    }

    #[test]
    fn resolves_an_arbitrarily_named_workspace_local_entry() {
        let workspace = tempdir().expect("workspace");
        let entry = workspace.path().join("scripts").join("anything.cmd");
        std::fs::create_dir_all(entry.parent().expect("parent")).expect("scripts");
        std::fs::write(&entry, "echo test").expect("entry");
        let resolved = resolve_program(
            "scripts/anything.cmd",
            workspace.path(),
            workspace.path(),
            &crate::tools::policy::PolicySettings::default(),
        )
        .expect("workspace entry resolves");
        assert_eq!(
            std::path::Path::new(&resolved),
            entry.canonicalize().unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_scripts_use_their_platform_runners() {
        let batch = command_for_program("C:/workspace/run-anything.cmd", &[]);
        assert_eq!(batch.as_std().get_program().to_string_lossy(), "cmd.exe");
        assert!(batch.as_std().get_args().any(|arg| arg == "/c"));
        assert_eq!(
            windows_batch_command_line(
                r"\\?\C:\workspace\Life Brain\run & tooling.cmd",
                &["argument & value".to_string()]
            ),
            r#"call "C:\workspace\Life Brain\run & tooling.cmd" "argument & value""#
        );

        let script = command_for_program("C:/workspace/run-anything.ps1", &[]);
        let runner = script
            .as_std()
            .get_program()
            .to_string_lossy()
            .to_ascii_lowercase();
        assert!(runner.contains("powershell") || runner.contains("pwsh"));
        assert!(script.as_std().get_args().any(|arg| arg == "-Command"));
    }

    #[cfg(windows)]
    #[test]
    #[serial_test::serial(process_runtime)]
    fn powershell_script_mode_prefers_pwsh_and_preserves_utf8_output() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "script": "Write-Output '中文輸出'",
                "shell": "powershell",
                "confirm": true,
                "timeout_ms": 10_000,
                "yield_time_ms": 10_000
            }),
        );

        assert_eq!(output["command_ok"], true, "{output}");
        assert!(
            output["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("中文輸出"),
            "{output}"
        );
        let environment = powershell_environment();
        assert_eq!(output["program"], environment["selected"], "{output}");
        if environment["pwsh_available"] == true {
            assert!(output["program"]
                .as_str()
                .unwrap_or_default()
                .to_ascii_lowercase()
                .contains("pwsh"));
        }
    }

    #[cfg(windows)]
    #[test]
    #[serial_test::serial(process_runtime)]
    fn windows_workspace_scripts_and_python_unicode_execute_successfully() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        std::fs::write(
            workspace.path().join("any-name.cmd"),
            "@echo tooling-cmd-ok\r\n",
        )
        .expect("cmd script");
        std::fs::write(
            workspace.path().join("any-name.ps1"),
            "Write-Output 'tooling-powershell-ok'\r\n",
        )
        .expect("powershell script");
        std::fs::write(
            workspace.path().join("workflow_probe.py"),
            "print('workflow-ok')\n",
        )
        .expect("python module");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        for command in [
            "any-name.cmd",
            "any-name.ps1",
            "cmd /c echo tooling-cmd-ok",
            "powershell -NoProfile -Command \"Write-Output tooling-powershell-ok\"",
            "python -c \"print('中文输出正常 ✅')\"",
        ] {
            let initial = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
            );
            assert_eq!(initial["ok"], true, "{command}: {initial}");
            assert_eq!(initial["startup"]["attempts"], 1, "{command}: {initial}");
            assert_eq!(
                initial["startup"]["error_dialog_suppressed"], true,
                "{command}: {initial}"
            );
            let output = if initial["process_still_running"] == true {
                call_tool(
                    &ctx,
                    "wait_command",
                    &json!({
                        "session_id": initial["session_id"],
                        "cursor": initial["next_cursor"],
                        "timeout_ms": 10_000,
                        "until": "finalized"
                    }),
                )
            } else {
                initial
            };
            assert_eq!(output["command_ok"], true, "{command}: {output}");
        }

        for _ in 0..10 {
            let initial = call_tool(
                &ctx,
                "exec_command",
                &json!({
                    "cmd": "python -m workflow_probe",
                    "timeout_ms": 10_000,
                    "yield_time_ms": 10_000
                }),
            );
            let initial_stdout = initial["stdout"].as_str().unwrap_or_default().to_string();
            let output = if initial["process_still_running"] == true {
                call_tool(
                    &ctx,
                    "wait_command",
                    &json!({
                        "session_id": initial["session_id"],
                        "cursor": initial["next_cursor"],
                        "timeout_ms": 10_000,
                        "until": "finalized"
                    }),
                )
            } else {
                initial
            };
            assert_eq!(output["command_ok"], true, "{output}");
            assert!(
                initial_stdout.contains("workflow-ok")
                    || output["stdout"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("workflow-ok"),
                "{output}"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    #[serial_test::serial(process_runtime)]
    fn windows_batch_scripts_preserve_space_paths_and_arguments() {
        let parent = tempdir().expect("workspace parent");
        let workspace = parent.path().join("Life Brain 中文");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx = ToolContext::for_test(workspace.clone(), harness.path().to_path_buf())
            .expect("context");

        for extension in ["cmd", "bat"] {
            let script_name = format!("run & tooling.{extension}");
            std::fs::write(
                workspace.join(&script_name),
                "@echo off\r\nif not \"%~1\"==\"argument & value\" exit /b 7\r\necho tooling-space-path-ok\r\n",
            )
            .expect("batch script");

            let command = format!(r#""{script_name}" "argument & value""#);
            let output = call_tool(
                &ctx,
                "exec_command",
                &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
            );
            assert_eq!(output["command_ok"], true, "{script_name}: {output}");
            let stdout = output["stdout"].as_str().unwrap_or_default();
            assert!(
                stdout.contains("tooling-space-path-ok"),
                "{script_name}: {output}"
            );
        }
    }

    #[cfg(windows)]
    fn delayed_output_command() -> &'static str {
        "cmd.exe /D /C \"(echo alpha)& ping -n 2 127.0.0.1 >nul & (echo beta)\""
    }

    #[cfg(unix)]
    fn delayed_output_command() -> &'static str {
        "sh -c \"printf 'alpha\\n'; sleep 1; printf 'beta\\n'\""
    }

    #[cfg(windows)]
    fn sleeping_command() -> &'static str {
        "powershell -NoProfile -Command \"Start-Sleep -Milliseconds 1200\""
    }

    #[cfg(unix)]
    fn sleeping_command() -> &'static str {
        "sh -c \"sleep 2\""
    }

    #[test]
    fn cargo_target_lock_uses_manifest_and_tauri_target_directories() {
        let workspace = tempdir().expect("workspace");
        let src_tauri = workspace.path().join("src-tauri");
        std::fs::create_dir_all(&src_tauri).expect("src-tauri");
        std::fs::write(
            src_tauri.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.1.0'\n",
        )
        .expect("manifest");
        let tauri = ExecSpec {
            display: "cargo tauri build".into(),
            program: "cargo".into(),
            args: vec!["tauri".into(), "build".into()],
            shell: "none".into(),
            env: Vec::new(),
            remove_env: Vec::new(),
        };
        let (_, tauri_target) = cargo_target_lock(&tauri, workspace.path()).expect("tauri lock");
        assert_eq!(
            std::path::Path::new(&tauri_target),
            src_tauri.join("target")
        );

        let manifest = ExecSpec {
            display: "cargo check --manifest-path src-tauri/Cargo.toml".into(),
            program: "cargo".into(),
            args: vec![
                "check".into(),
                "--manifest-path".into(),
                "src-tauri/Cargo.toml".into(),
            ],
            shell: "none".into(),
            env: Vec::new(),
            remove_env: Vec::new(),
        };
        let (manifest_group, manifest_target) =
            cargo_target_lock(&manifest, workspace.path()).expect("manifest lock");
        assert_eq!(
            std::path::Path::new(&manifest_target),
            src_tauri.join("target")
        );
        let (tauri_group, _) = cargo_target_lock(&tauri, workspace.path()).expect("tauri lock");
        assert_eq!(manifest_group, tauri_group);
    }

    #[test]
    fn automatic_cargo_dedupe_uses_request_shape_after_executable_resolution() {
        let workspace = tempdir().expect("workspace");
        let spec = ExecSpec {
            display:
                r"C:\Users\tester\.cargo\bin\cargo.exe test --manifest-path src-tauri/Cargo.toml"
                    .into(),
            program: r"C:\Users\tester\.cargo\bin\cargo.exe".into(),
            args: vec![
                "test".into(),
                "--manifest-path".into(),
                "src-tauri/Cargo.toml".into(),
            ],
            shell: "none".into(),
            env: Vec::new(),
            remove_env: Vec::new(),
        };
        let request = json!({
            "program": "cargo",
            "args": ["test", "--manifest-path", "src-tauri/Cargo.toml"]
        });

        let identity =
            execution_identity(&request, &spec, workspace.path(), 30_000, false, "", &[]);

        assert!(
            identity
                .operation_id
                .as_deref()
                .is_some_and(|operation_id| operation_id.starts_with("auto:")),
            "resolved cargo.exe paths must not disable automatic deduplication: {identity:?}"
        );
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn duplicate_operations_reattach_and_ignore_legacy_wait_heartbeats() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let request = json!({
            "cmd": sleeping_command(),
            "operation_id": "dedupe-regression",
            "timeout_ms": 5000,
            "yield_time_ms": 0,
            "output_mode": "none"
        });
        let started = call_tool(&ctx, "exec_command", &request);
        assert_eq!(started["process_still_running"], true, "{started}");
        assert_eq!(started["deduplicated"], false, "{started}");
        let session_id = started["session_id"].as_str().expect("session id");

        let duplicate = call_tool(&ctx, "exec_command", &request);
        assert_eq!(duplicate["deduplicated"], true, "{duplicate}");
        assert_eq!(duplicate["session_id"], session_id, "{duplicate}");
        assert_eq!(
            duplicate["attached_to_session_id"], session_id,
            "{duplicate}"
        );

        let resolved = call_tool(
            &ctx,
            "resolve_operation",
            &json!({"operation_id": "dedupe-regression", "output_mode": "none"}),
        );
        assert_eq!(resolved["session_id"], session_id, "{resolved}");
        assert_eq!(resolved["deduplicated"], true, "{resolved}");

        let listed = call_tool(
            &ctx,
            "list_sessions",
            &json!({"include_finalized": false, "limit": 10}),
        );
        assert!(listed["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .any(|session| session["session_id"] == session_id));

        let waited = call_tool(
            &ctx,
            "wait_command",
            &json!({
                "session_id": session_id,
                "cursor": started["next_cursor"],
                "timeout_ms": 1000,
                "heartbeat_ms": 25,
                "until": "finalized",
                "output_mode": "none"
            }),
        );
        assert_eq!(waited["heartbeat"], false, "{waited}");
        assert_eq!(waited["request_timed_out"], true, "{waited}");
        assert_eq!(waited["effective_wait_ms"], 1000, "{waited}");
        assert!(
            waited["actual_wait_ms"].as_u64().unwrap_or(0) >= 900,
            "{waited}"
        );
        assert_eq!(waited["process_still_running"], true, "{waited}");
        assert_eq!(
            waited["next_actions"][0]["arguments"]["session_id"], session_id,
            "{waited}"
        );
        assert!(
            waited["next_actions"][0]["arguments"]
                .get("heartbeat_ms")
                .is_none(),
            "{waited}"
        );

        let conflict = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": delayed_output_command(),
                "operation_id": "dedupe-regression",
                "timeout_ms": 5000,
                "yield_time_ms": 0,
                "output_mode": "none"
            }),
        );
        assert_eq!(conflict["ok"], false, "{conflict}");
        assert_eq!(
            conflict["error"]["code"], "OPERATION_ID_CONFLICT",
            "{conflict}"
        );

        let killed = call_tool(
            &ctx,
            "kill_session",
            &json!({"session_id": session_id, "wait_ms": 5000}),
        );
        assert_eq!(killed["process_still_running"], false, "{killed}");
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn wait_command_returns_only_new_sequence_events() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": delayed_output_command(),
                "timeout_ms": 5000,
                "yield_time_ms": 0,
                "output_mode": "delta"
            }),
        );
        let session_id = started["session_id"].as_str().expect("session id");
        let first = call_tool(
            &ctx,
            "wait_command",
            &json!({
                "session_id": session_id,
                "cursor": 0,
                "timeout_ms": 3000,
                "until": "output_or_exit",
                "output_mode": "delta"
            }),
        );
        assert_eq!(first["request_timed_out"], false, "{first}");
        assert!(
            first["session_registry_wait_ms"].as_u64().is_some(),
            "{first}"
        );
        assert!(first["actual_wait_ms"].as_u64().is_some(), "{first}");
        assert!(first["snapshot_ms"].as_u64().is_some(), "{first}");
        assert!(
            first["stdout"]
                .as_str()
                .unwrap_or_default()
                .contains("alpha"),
            "{first}"
        );
        let cursor = first["next_cursor"].as_u64().expect("cursor");

        let second = call_tool(
            &ctx,
            "wait_command",
            &json!({
                "session_id": session_id,
                "cursor": cursor,
                "timeout_ms": 5000,
                "until": "finalized",
                "output_mode": "delta"
            }),
        );
        let stdout = second["stdout"].as_str().unwrap_or_default();
        assert!(stdout.contains("beta"), "{second}");
        assert!(
            !stdout.contains("alpha"),
            "old output must not repeat: {second}"
        );
        assert!(
            second["next_cursor"].as_u64().unwrap_or(0) > cursor,
            "{second}"
        );
        assert_eq!(second["process_still_running"], false, "{second}");
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn wait_timeout_does_not_become_process_timeout() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": sleeping_command(),
                "timeout_ms": 5000,
                "yield_time_ms": 0,
                "output_mode": "none"
            }),
        );
        let session_id = started["session_id"].as_str().expect("session id");
        let waited = call_tool(
            &ctx,
            "wait_command",
            &json!({
                "session_id": session_id,
                "cursor": 0,
                "timeout_ms": 50,
                "until": "output_or_exit",
                "output_mode": "none"
            }),
        );
        assert_eq!(waited["request_timed_out"], true, "{waited}");
        assert_eq!(waited["process_timed_out"], false, "{waited}");
        assert_eq!(waited["process_still_running"], true, "{waited}");
        let _ = call_tool(
            &ctx,
            "kill_session",
            &json!({"session_id": session_id, "wait_ms": 5000}),
        );
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn process_timeout_is_reported_separately() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        let started = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": sleeping_command(),
                "timeout_ms": 100,
                "yield_time_ms": 0,
                "output_mode": "none"
            }),
        );
        let session_id = started["session_id"].as_str().expect("session id");
        let finished = call_tool(
            &ctx,
            "wait_command",
            &json!({
                "session_id": session_id,
                "timeout_ms": 5000,
                "until": "finalized",
                "output_mode": "none"
            }),
        );
        assert_eq!(finished["request_timed_out"], false, "{finished}");
        assert_eq!(finished["process_timed_out"], true, "{finished}");
        assert_eq!(finished["process_still_running"], false, "{finished}");
        assert_eq!(
            finished["termination_reason"], "process_timeout",
            "{finished}"
        );
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn post_checks_are_part_of_final_command_success() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        #[cfg(windows)]
        let main_command = "cmd /d /c echo main-ok";
        #[cfg(unix)]
        let main_command = "sh -c \"printf main-ok\"";
        #[cfg(windows)]
        let passing_check = "cmd /d /c echo verify-ok";
        #[cfg(unix)]
        let passing_check = "sh -c \"printf verify-ok\"";
        #[cfg(windows)]
        let failing_check = "cmd /d /c exit 7";
        #[cfg(unix)]
        let failing_check = "sh -c \"exit 7\"";

        let passed = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": main_command,
                "timeout_ms": 5000,
                "yield_time_ms": 5000,
                "post_checks": [{"name": "verify", "cmd": passing_check}]
            }),
        );
        assert_eq!(passed["execution_ok"], true, "{passed}");
        assert_eq!(passed["verification_ok"], true, "{passed}");
        assert_eq!(passed["command_ok"], true, "{passed}");
        assert_eq!(
            passed["post_checks"]["results"][0]["passed"], true,
            "{passed}"
        );
        assert_eq!(
            passed["post_checks"]["execution_mode"], "parallel",
            "{passed}"
        );
        assert_eq!(passed["post_checks"]["max_concurrency"], 1, "{passed}");
        assert_eq!(
            passed["post_checks"]["results"][0]["startup"]["attempts"], 1,
            "{passed}"
        );

        let failed = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": main_command,
                "timeout_ms": 5000,
                "yield_time_ms": 5000,
                "post_checks": [{"name": "verify", "cmd": failing_check}]
            }),
        );
        assert_eq!(failed["execution_ok"], true, "{failed}");
        assert_eq!(failed["verification_ok"], false, "{failed}");
        assert_eq!(failed["command_ok"], false, "{failed}");
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn exec_returns_after_first_output_while_process_is_running() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        #[cfg(windows)]
        let command =
            "powershell -NoProfile -Command \"Write-Output ready; Start-Sleep -Seconds 3\"";
        #[cfg(unix)]
        let command = "sh -c \"printf ready; sleep 3\"";

        let result = exec_command_async(
            &ctx,
            &json!({
                "cmd": command,
                "timeout_ms": 10_000,
                "yield_time_ms": 5_000,
                "output_mode": "tail"
            }),
        )
        .await
        .expect("exec result");

        assert!(result["first_output_ms"].as_u64().is_some(), "{result}");
        assert!(result["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("ready"));
        assert_eq!(result["process_still_running"], true, "{result}");
        let session_id = result["session_id"].as_str().expect("session id");
        let _ = crate::tools::session::kill_session_async(
            &ctx.sessions,
            &json!({"session_id": session_id, "wait_ms": 5000}),
        )
        .await;
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn permission_grant_resumes_original_operation() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");

        #[cfg(windows)]
        let shell = "powershell";
        #[cfg(windows)]
        let command = "Write-Output permission-resumed";
        #[cfg(unix)]
        let shell = "sh";
        #[cfg(unix)]
        let command = "printf permission-resumed";

        let blocked = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": command,
                "shell": shell,
                "timeout_ms": 5000,
                "yield_time_ms": 5000
            }),
        );
        assert_eq!(blocked["ok"], false, "{blocked}");
        let resume_id = blocked["error"]["details"]["permission_request"]["resume_id"]
            .as_str()
            .expect("resume id");
        let denied = call_tool(
            &ctx,
            "request_permissions",
            &json!({"resume_id": resume_id, "approve": true, "scope": "once"}),
        );
        assert_eq!(denied["ok"], false, "{denied}");
        assert_eq!(
            denied["error"]["code"], "PERMISSION_NOT_APPROVED",
            "{denied}"
        );
        let resumed = call_tool(
            &ctx,
            "request_permissions",
            &json!({
                "resume_id": resume_id,
                "approve": true,
                "confirm": true,
                "scope": "once"
            }),
        );
        assert_eq!(resumed["ok"], true, "{resumed}");
        assert_eq!(resumed["resumed"], true, "{resumed}");
        assert_eq!(resumed["command_ok"], true, "{resumed}");
        assert!(resumed["stdout"]
            .as_str()
            .unwrap_or_default()
            .contains("permission-resumed"));
    }

    #[test]
    #[serial_test::serial(process_runtime)]
    fn output_modes_none_and_summary_reduce_payload() {
        let workspace = tempdir().expect("workspace");
        let harness = tempdir().expect("harness");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("context");
        #[cfg(windows)]
        let command = "powershell -NoProfile -Command \"Write-Output repeated; Write-Output repeated; Write-Output final\"";
        #[cfg(unix)]
        let command = "sh -c \"printf 'repeated\\nrepeated\\nfinal\\n'\"";

        let none = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": command,
                "timeout_ms": 5000,
                "yield_time_ms": 5000,
                "output_mode": "none"
            }),
        );
        assert_eq!(none["command_ok"], true, "{none}");
        assert_eq!(none["stdout"], "", "{none}");
        assert!(none["output_refs"]["stdout"].is_string(), "{none}");

        let summary = call_tool(
            &ctx,
            "exec_command",
            &json!({
                "cmd": command,
                "timeout_ms": 5000,
                "yield_time_ms": 5000,
                "output_mode": "summary",
                "tail_lines": 10
            }),
        );
        let stdout = summary["stdout"].as_str().unwrap_or_default();
        assert_eq!(stdout.matches("repeated").count(), 1, "{summary}");
        assert!(stdout.contains("final"), "{summary}");
    }

    #[cfg(unix)]
    #[test]
    fn unix_workspace_scripts_preserve_space_paths_and_arguments() {
        use std::os::unix::fs::PermissionsExt;

        let parent = tempdir().expect("workspace parent");
        let workspace = parent.path().join("Life Brain 中文");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let harness = tempdir().expect("harness");
        let script_name = "run tooling";
        let script_path = workspace.join(script_name);
        std::fs::write(
            &script_path,
            "#!/bin/sh\nprintf 'tooling-space-path-ok\\n'\nprintf 'argument=[%s]\\n' \"$1\"\n",
        )
        .expect("shell script");
        let mut permissions = std::fs::metadata(&script_path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script_path, permissions).expect("script executable");

        let ctx = ToolContext::for_test(workspace, harness.path().to_path_buf()).expect("context");
        let command = format!(r#""{script_name}" "argument with spaces""#);
        let output = call_tool(
            &ctx,
            "exec_command",
            &json!({ "cmd": command, "timeout_ms": 10_000, "yield_time_ms": 10_000 }),
        );
        assert_eq!(output["command_ok"], true, "{output}");
        let stdout = output["stdout"].as_str().unwrap_or_default();
        assert!(stdout.contains("tooling-space-path-ok"), "{output}");
        assert!(
            stdout.contains("argument=[argument with spaces]"),
            "{output}"
        );
    }
}

fn command_for_program(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let extension = Path::new(program)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("bat") | Some("cmd") => {
                let mut command = Command::new("cmd.exe");
                command.args(["/d", "/s", "/c"]);
                command
                    .as_std_mut()
                    .raw_arg(windows_batch_command_line(program, args));
                return command;
            }
            Some("ps1") => {
                let shell = detected_powershell()
                    .map(|runtime| PathBuf::from(&runtime.program))
                    .unwrap_or_else(|| PathBuf::from("powershell.exe"));
                let mut command = Command::new(shell);
                let mut invocation = format!(
                    "& {}",
                    powershell_literal(windows_command_path(program).as_str())
                );
                for argument in args {
                    invocation.push(' ');
                    invocation.push_str(&powershell_literal(argument));
                }
                let script = powershell_script(&invocation);
                command.args([
                    "-NoLogo",
                    "-NoProfile",
                    "-NonInteractive",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    script.as_str(),
                ]);
                return command;
            }
            _ => {}
        }
    }

    let mut command = Command::new(program);
    command.args(args);
    command
}

#[cfg(windows)]
fn windows_batch_command_line(program: &str, args: &[String]) -> String {
    let mut command_line = String::from("call ");
    command_line.push_str(&windows_batch_token(&windows_command_path(program)));
    for arg in args {
        command_line.push(' ');
        command_line.push_str(&windows_batch_token(arg));
    }
    command_line
}

#[cfg(windows)]
fn windows_batch_token(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn platform_command_path(path: &Path) -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::path::PathBuf::from(windows_command_path(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}

#[cfg(windows)]
fn windows_command_path(path: &str) -> String {
    path.strip_prefix("\\\\?\\").unwrap_or(path).to_string()
}
