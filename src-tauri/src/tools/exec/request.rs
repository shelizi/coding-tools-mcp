use std::path::PathBuf;

use coding_tools_command_policy::resolved_command_timeout_ms as resolve_command_timeout_ms;
use serde_json::{json, Value};

use crate::mcp::command_kind;
use crate::tools::context::{RuntimeToolConfig, ToolContext};
use crate::tools::redaction::arguments_reference_sensitive_source;
use crate::tools::session::{OutputMode, OutputOptions};
use crate::tools::workspace::WorkspaceError;
use crate::tools::{ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, DEFAULT_COMMAND_TIMEOUT_MAX_MS};
use crate::workspace::SecurityPolicy;

use super::spec::{
    resolution_target_for_sandbox, resolve_exec_spec_for_target, resolve_post_checks_for_target,
    ExecSpec, PostCheckSpec,
};

pub(super) struct ResolvedExecRequest {
    pub(super) workdir: PathBuf,
    pub(super) filesystem_scope: String,
    pub(super) spec: ExecSpec,
    pub(super) post_checks: Vec<PostCheckSpec>,
    pub(super) output_options: OutputOptions,
    pub(super) legacy_native: bool,
}

pub(super) struct ExecRuntimeOptions<'a> {
    pub(super) timeout_ms: u64,
    pub(super) yield_ms: u64,
    pub(super) tty: bool,
    pub(super) stdin_text: &'a str,
    pub(super) sensitive_output: bool,
}

pub(super) fn resolve_exec_request(
    ctx: &ToolContext,
    args: &Value,
    runtime: &RuntimeToolConfig,
) -> Result<ResolvedExecRequest, WorkspaceError> {
    let workdir_raw = args
        .get("workdir")
        .or_else(|| args.get("cwd"))
        .and_then(Value::as_str)
        .unwrap_or(".");
    let workdir = if runtime.policy.security_policy.enforce_workspace_boundary {
        ctx.workspace.resolve_existing(workdir_raw)?
    } else {
        let requested = PathBuf::from(workdir_raw);
        let path = if requested.is_absolute() {
            requested
        } else {
            ctx.default_cwd_path().join(requested)
        };
        crate::tools::workspace::ResolvedPath {
            display: path.to_string_lossy().into_owned(),
            path: path.canonicalize().unwrap_or(path),
            existed: true,
        }
    };
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
    validate_child_process_scope(args, &runtime.policy.security_policy)?;
    let resolution_target = resolution_target_for_sandbox(&runtime.sandbox);
    let spec = resolve_exec_spec_for_target(
        args,
        &workdir.path,
        ctx.workspace.root(),
        &runtime.policy,
        resolution_target,
    )?;
    let post_checks = resolve_post_checks_for_target(
        args,
        &workdir.path,
        ctx.workspace.root(),
        &runtime.policy,
        resolution_target,
    )?;
    let output_options = OutputOptions::from_args(args, OutputMode::Tail);
    let legacy_native = !ctx.workspace.is_wsl()
        && args.get("program").is_none()
        && spec.shell == "none"
        && spec.env.is_empty()
        && spec.remove_env.is_empty()
        && post_checks.is_empty();

    Ok(ResolvedExecRequest {
        workdir: workdir.path,
        filesystem_scope,
        spec,
        post_checks,
        output_options,
        legacy_native,
    })
}

pub(super) fn resolve_runtime_options<'a>(
    args: &'a Value,
    spec: &ExecSpec,
    security_policy: &SecurityPolicy,
) -> ExecRuntimeOptions<'a> {
    ExecRuntimeOptions {
        timeout_ms: resolved_command_timeout_ms(args, spec),
        yield_ms: args
            .get("yield_time_ms")
            .and_then(Value::as_u64)
            .unwrap_or(1000)
            .min(30_000),
        tty: args.get("tty").and_then(Value::as_bool).unwrap_or(false),
        stdin_text: args.get("stdin").and_then(Value::as_str).unwrap_or(""),
        sensitive_output: security_policy.withhold_sensitive_source_output
            && arguments_reference_sensitive_source(args),
    }
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

fn validate_child_process_scope(
    args: &Value,
    security_policy: &SecurityPolicy,
) -> Result<(), WorkspaceError> {
    let scope = args
        .get("filesystem_scope")
        .and_then(Value::as_str)
        .unwrap_or("workspace");
    match scope {
        "workspace" => Ok(()),
        "host" if !security_policy.enforce_workspace_boundary => Ok(()),
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
