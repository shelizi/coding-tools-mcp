use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::process::Command as TokioCommand;
use uuid::Uuid;

use crate::tools::process_child::{ProcessChild, ProcessKillHook};
use crate::tools::process_spec::ProcessLaunchSpec;
use crate::workspace::{SandboxPathAccess, SandboxPathGrant};

use crate::tools::workspace::{Workspace, WorkspaceError, WorkspaceResult};

use super::{PreparedSandbox, SandboxCommand, SandboxProcessPlan};

const BACKEND_ID: &str = "docker_sbx";
const SANDBOX_NAME_PREFIX: &str = "ctmcp-";
const REMOTE_SUPERVISOR_SCRIPT: &str = r#"pidfile=$1; inner=$2; shift 2; setsid -w sh -c "$inner" ctmcp-inner "$pidfile" "$@"; status=$?; rm -f "$pidfile"; exit $status"#;
const REMOTE_INNER_SCRIPT: &str =
    r#"pidfile=$1; shift; printf '%s\n' "$$" > "$pidfile"; exec "$@""#;
const REMOTE_KILL_SCRIPT: &str = r#"pidfile=$1; i=0; while [ ! -s "$pidfile" ] && [ "$i" -lt 50 ]; do sleep 0.1; i=$((i+1)); done; [ -s "$pidfile" ] || exit 0; pid=$(cat "$pidfile") || exit 6; if kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null; then sleep 1; kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true; exit 0; fi; kill -0 "$pid" 2>/dev/null || exit 0; exit 5"#;

#[derive(Debug, Clone, PartialEq, Eq)]
struct SbxMount {
    path: PathBuf,
    access: SandboxPathAccess,
}

pub(crate) fn discover_sbx_program() -> Option<PathBuf> {
    if let Ok(path) = which::which("sbx") {
        return Some(path);
    }

    #[cfg(windows)]
    {
        if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
            let candidate = PathBuf::from(local_app_data)
                .join("DockerSandboxes")
                .join("bin")
                .join("sbx.exe");
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

pub(super) fn prepare(
    workspace: &Workspace,
    grants: &[SandboxPathGrant],
) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
    let cli = discover_sbx_program().ok_or_else(|| {
        sbx_error(
            "SANDBOX_SBX_UNAVAILABLE",
            "Docker Sandboxes sbx CLI was not found.",
            "discovery",
            "Install the standalone Docker Sandboxes sbx CLI and ensure it is available on PATH.",
        )
    })?;
    let mounts = build_mounts(workspace.root(), grants)?;
    let sandbox_name = sandbox_name(&mounts);
    ensure_sandbox(&cli, &sandbox_name, &mounts)?;
    ensure_remote_supervisor(&cli, &sandbox_name)?;

    Ok(Box::new(DockerSbxPreparedSandbox {
        cli,
        sandbox_name,
        mounts,
    }))
}

struct DockerSbxPreparedSandbox {
    cli: PathBuf,
    sandbox_name: String,
    mounts: Vec<SbxMount>,
}

impl PreparedSandbox for DockerSbxPreparedSandbox {
    fn backend_id(&self) -> &str {
        BACKEND_ID
    }

    fn normalize_logical_command(
        &self,
        mut command: SandboxCommand,
    ) -> WorkspaceResult<SandboxCommand> {
        let cwd = canonical_existing_directory(&command.cwd, "command working directory")?;
        if !self.path_is_mounted(&cwd) {
            return Err(sbx_error(
                "SANDBOX_SBX_PATH_UNMOUNTED",
                format!(
                    "Command working directory is not mounted in the Docker sandbox: {}",
                    command.cwd.display()
                ),
                "prepare_command",
                "Run inside the workspace or add the directory as an explicit sandbox path grant.",
            ));
        }
        command.cwd = cwd;

        if windows_only_program(&command.executable) {
            return Err(sbx_error(
                "SANDBOX_SBX_COMMAND_UNSUPPORTED",
                format!(
                    "Windows host executable cannot run in the Docker Linux sandbox: {}",
                    command.executable.display()
                ),
                "prepare_command",
                "Use a Linux-side command name such as python, node, git, sh, bash, or cargo.",
            ));
        }

        if command.executable.is_absolute() {
            let executable = command.executable.canonicalize().map_err(|error| {
                sbx_error(
                    "SANDBOX_SBX_COMMAND_UNAVAILABLE",
                    format!(
                        "Sandbox command path is unavailable: {}: {error}",
                        command.executable.display()
                    ),
                    "prepare_command",
                    "Use a command installed inside the sandbox or a workspace-local executable.",
                )
            })?;
            if !executable.is_file() || !self.path_is_mounted(&executable) {
                return Err(sbx_error(
                    "SANDBOX_SBX_COMMAND_UNMOUNTED",
                    format!("Sandbox command path is outside mounted workspaces: {}", command.executable.display()),
                    "prepare_command",
                    "Use a command installed inside the sandbox or a mounted workspace-local executable.",
                ));
            }
            command.executable = executable;
        } else if command.executable.components().count() > 1 {
            return Err(sbx_error(
                "SANDBOX_SBX_COMMAND_UNAVAILABLE",
                format!(
                    "Relative sandbox command path was not resolved inside a mounted workspace: {}",
                    command.executable.display()
                ),
                "prepare_command",
                "Use a bare command name or an existing workspace-local executable.",
            ));
        }
        Ok(command)
    }

    fn prepare_command(
        &self,
        command: SandboxCommand,
        env: Vec<(String, String)>,
        remove_env: Vec<String>,
    ) -> WorkspaceResult<SandboxProcessPlan> {
        let command = self.normalize_logical_command(command)?;
        self.prepare_process(ProcessLaunchSpec {
            program: command.executable,
            args: command.args,
            cwd: Some(command.cwd),
            env,
            remove_env,
            required_env: Vec::new(),
            windows_raw_arg: None,
            using_wsl: false,
        })
    }

    fn prepare_process(
        &self,
        mut process: ProcessLaunchSpec,
    ) -> WorkspaceResult<SandboxProcessPlan> {
        if process.using_wsl || process.windows_raw_arg.is_some() {
            return Err(sbx_error(
                "SANDBOX_SBX_PROCESS_UNSUPPORTED",
                "Windows/WSL host process normalization cannot be forwarded into a Docker Linux sandbox.",
                "prepare_process",
                "Use a Linux-side command supported by Docker Sandboxes.",
            ));
        }
        if let Some(cwd) = process.cwd.as_ref() {
            let canonical = canonical_existing_directory(cwd, "command working directory")?;
            if !self.path_is_mounted(&canonical) {
                return Err(sbx_error(
                    "SANDBOX_SBX_PATH_UNMOUNTED",
                    format!("Command working directory is not mounted in the Docker sandbox: {}", cwd.display()),
                    "prepare_process",
                    "Run inside the workspace or add the directory as an explicit sandbox path grant.",
                ));
            }
            process.cwd = Some(canonical);
        }
        Ok(SandboxProcessPlan {
            backend_id: BACKEND_ID.into(),
            process,
            environment_overrides: BTreeMap::new(),
            state: None,
        })
    }

    fn launch_prepared_process(&self, plan: SandboxProcessPlan) -> WorkspaceResult<ProcessChild> {
        if plan.backend_id != BACKEND_ID {
            return Err(sbx_error(
                "SANDBOX_PROCESS_PLAN_INVALID",
                format!(
                    "Prepared process backend '{}' does not match '{}'.",
                    plan.backend_id, BACKEND_ID
                ),
                "launch",
                "Rebuild the process plan with the selected sandbox backend.",
            ));
        }
        let pidfile = format!("/tmp/ctmcp-{}.pid", Uuid::new_v4().simple());
        let args = build_exec_args(&self.sandbox_name, &plan, &pidfile)?;
        let mut command = TokioCommand::new(&self.cli);
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = command.spawn().map_err(|error| {
            sbx_error(
                "SANDBOX_SBX_EXEC_FAILED",
                format!("Failed to start sbx exec: {error}"),
                "launch",
                "Verify Docker Sandboxes is running and the selected sandbox can be started.",
            )
        })?;
        let kill_hook = remote_kill_hook(self.cli.clone(), self.sandbox_name.clone(), pidfile);
        // The host Job Object contains only the sbx CLI client. The actual command runs
        // inside the microVM. The remote kill hook manages our supervised process group,
        // but an adversarial command may create a new session, so do not claim complete
        // microVM process-tree containment.
        Ok(ProcessChild::from_tokio(child)
            .with_process_tree_contained(false)
            .with_kill_hook(kill_hook))
    }
}

impl DockerSbxPreparedSandbox {
    fn path_is_mounted(&self, path: &Path) -> bool {
        self.mounts
            .iter()
            .any(|mount| path.starts_with(&mount.path))
    }
}

fn canonical_existing_directory(path: &Path, label: &str) -> WorkspaceResult<PathBuf> {
    let canonical = path.canonicalize().map_err(|error| {
        sbx_error(
            "SANDBOX_SBX_PATH_INVALID",
            format!("{label} is unavailable: {}: {error}", path.display()),
            "mounts",
            "Use an existing local directory.",
        )
    })?;
    if !canonical.is_dir() {
        return Err(sbx_error(
            "SANDBOX_SBX_PATH_INVALID",
            format!("{label} must be a directory: {}", path.display()),
            "mounts",
            "Docker Sandboxes workspace grants must reference directories.",
        ));
    }
    if is_unsupported_remote_path(&canonical) {
        return Err(sbx_error(
            "SANDBOX_SBX_PATH_UNSUPPORTED",
            format!(
                "Network-backed workspace paths are not supported by this adapter: {}",
                path.display()
            ),
            "mounts",
            "Use a local filesystem directory or a WSL folder path (\\\\wsl.localhost\\<distro>\\...).",
        ));
    }
    Ok(canonical)
}

fn build_mounts(
    workspace_root: &Path,
    grants: &[SandboxPathGrant],
) -> WorkspaceResult<Vec<SbxMount>> {
    let workspace = canonical_existing_directory(workspace_root, "workspace root")?;
    let mut external = BTreeMap::<PathBuf, SandboxPathAccess>::new();
    for grant in grants {
        let raw = PathBuf::from(grant.path.trim());
        if grant.path.trim().is_empty() {
            continue;
        }
        let path = canonical_existing_directory(&raw, "external sandbox path")?;
        if path.starts_with(&workspace) {
            continue;
        }
        if workspace.starts_with(&path) {
            return Err(sbx_error(
                "SANDBOX_SBX_MOUNT_OVERLAP",
                format!(
                    "External sandbox path contains the primary workspace: {}",
                    raw.display()
                ),
                "mounts",
                "Grant a sibling directory instead of an ancestor of the primary workspace.",
            ));
        }
        external
            .entry(path)
            .and_modify(|current| {
                if grant.access == SandboxPathAccess::Modify {
                    *current = SandboxPathAccess::Modify;
                }
            })
            .or_insert(grant.access);
    }

    let external_entries = external.iter().collect::<Vec<_>>();
    for (parent_path, parent_access) in &external_entries {
        if **parent_access != SandboxPathAccess::Modify {
            continue;
        }
        for (child_path, child_access) in &external_entries {
            if parent_path != child_path
                && child_path.starts_with(parent_path)
                && **child_access == SandboxPathAccess::ReadOnly
            {
                return Err(sbx_error(
                    "SANDBOX_SBX_MOUNT_OVERLAP",
                    format!(
                        "Writable external path contains a read-only grant, which cannot be enforced safely: {} contains {}",
                        parent_path.display(),
                        child_path.display()
                    ),
                    "mounts",
                    "Remove the broader writable grant or make the nested grant writable too.",
                ));
            }
        }
    }

    let mut mounts = vec![SbxMount {
        path: workspace,
        access: SandboxPathAccess::Modify,
    }];
    mounts.extend(
        external
            .into_iter()
            .map(|(path, access)| SbxMount { path, access }),
    );
    Ok(mounts)
}

fn sandbox_name(mounts: &[SbxMount]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"coding-tools-mcp/docker-sbx/v1\0");
    for mount in mounts {
        digest.update(host_mount_path(&mount.path).as_bytes());
        digest.update(match mount.access {
            SandboxPathAccess::ReadOnly => b"\0ro\0".as_slice(),
            SandboxPathAccess::Modify => b"\0rw\0".as_slice(),
        });
    }
    let hash = format!("{:x}", digest.finalize());
    format!("{SANDBOX_NAME_PREFIX}{}", &hash[..24])
}

fn ensure_sandbox(cli: &Path, name: &str, mounts: &[SbxMount]) -> WorkspaceResult<()> {
    static CREATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = CREATE_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| {
            sbx_error(
                "SANDBOX_SBX_CREATE_FAILED",
                "Docker sandbox creation lock is poisoned.",
                "create",
                "Restart the application and retry.",
            )
        })?;

    let listed = run_sbx(cli, ["ls", "--json"])?;
    if !listed.status.success() {
        return Err(sbx_output_error(
            "SANDBOX_SBX_LIST_FAILED",
            "Unable to list Docker Sandboxes.",
            "list",
            &listed,
        ));
    }
    let value: Value = serde_json::from_slice(&listed.stdout).map_err(|error| {
        sbx_error(
            "SANDBOX_SBX_LIST_FAILED",
            format!("Docker Sandboxes returned invalid JSON: {error}"),
            "list",
            "Update the sbx CLI or verify it runs successfully from a terminal.",
        )
    })?;
    if listed_sandbox_exists(&value, name) {
        return Ok(());
    }

    let mut args = vec![
        "create".to_string(),
        "-q".to_string(),
        "--name".to_string(),
        name.to_string(),
        "shell".to_string(),
    ];
    for mount in mounts {
        let mut value = host_mount_path(&mount.path);
        if mount.access == SandboxPathAccess::ReadOnly {
            value.push_str(":ro");
        }
        args.push(value);
    }
    let created = run_sbx(cli, args.iter().map(String::as_str))?;
    if !created.status.success() {
        return Err(classify_create_error(&created));
    }
    Ok(())
}

fn ensure_remote_supervisor(cli: &Path, name: &str) -> WorkspaceResult<()> {
    let output = run_sbx(
        cli,
        [
            "exec",
            name,
            "sh",
            "-c",
            "command -v setsid >/dev/null 2>&1 && setsid -w true",
        ],
    )?;
    if output.status.success() {
        return Ok(());
    }
    Err(sbx_output_error_with_suggestion(
        "SANDBOX_SBX_SUPERVISOR_UNAVAILABLE",
        "Docker Sandbox does not provide the process-group supervisor required for reliable cancellation.",
        "supervisor",
        &output,
        "Use a Docker Sandboxes shell environment that provides sh and setsid with -w support.",
    ))
}

fn remote_kill_hook(cli: PathBuf, sandbox_name: String, pidfile: String) -> ProcessKillHook {
    Arc::new(move || cancel_remote_process(&cli, &sandbox_name, &pidfile))
}

fn cancel_remote_process(cli: &Path, name: &str, pidfile: &str) -> io::Result<()> {
    let output = Command::new(cli)
        .args([
            "exec",
            name,
            "sh",
            "-c",
            REMOTE_KILL_SCRIPT,
            "ctmcp-kill",
            pidfile,
        ])
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "sbx remote cancellation failed with status {:?}: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn run_sbx<'a>(cli: &Path, args: impl IntoIterator<Item = &'a str>) -> WorkspaceResult<Output> {
    Command::new(cli).args(args).output().map_err(|error| {
        sbx_error(
            "SANDBOX_SBX_UNAVAILABLE",
            format!("Failed to execute Docker Sandboxes sbx CLI: {error}"),
            "cli",
            "Install or repair the standalone sbx CLI.",
        )
    })
}

fn listed_sandbox_exists(value: &Value, name: &str) -> bool {
    value
        .get("sandboxes")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                item.get("name")
                    .or_else(|| item.get("Name"))
                    .and_then(Value::as_str)
                    == Some(name)
            })
        })
}

fn classify_create_error(output: &Output) -> WorkspaceError {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("global network policy has not been initialized") {
        return sbx_output_error_with_suggestion(
            "SANDBOX_SBX_SETUP_REQUIRED",
            "Docker Sandboxes network policy has not been initialized.",
            "create",
            output,
            "Choose a Docker Sandboxes network policy explicitly with: sbx policy init <allow-all|balanced|deny-all>.",
        );
    }
    if lower.contains("login") || lower.contains("sign in") || lower.contains("not authenticated") {
        return sbx_output_error_with_suggestion(
            "SANDBOX_SBX_SETUP_REQUIRED",
            "Docker Sandboxes requires authentication before this sandbox can be created.",
            "create",
            output,
            "Run sbx login, then retry.",
        );
    }
    sbx_output_error(
        "SANDBOX_SBX_CREATE_FAILED",
        "Docker Sandbox creation failed.",
        "create",
        output,
    )
}

fn build_exec_args(
    name: &str,
    plan: &SandboxProcessPlan,
    pidfile: &str,
) -> WorkspaceResult<Vec<String>> {
    let cwd = plan.process.cwd.as_ref().ok_or_else(|| {
        sbx_error(
            "SANDBOX_PROCESS_PLAN_INVALID",
            "Docker sandbox process plan requires a working directory.",
            "launch",
            "Provide a workspace working directory.",
        )
    })?;
    let program = if plan.process.program.is_absolute() {
        sandbox_runtime_path(&plan.process.program)
    } else {
        plan.process.program.to_string_lossy().into_owned()
    };
    if program.trim().is_empty() {
        return Err(sbx_error(
            "SANDBOX_PROCESS_PLAN_INVALID",
            "Docker sandbox process plan has an empty command.",
            "launch",
            "Provide a command to execute inside the sandbox.",
        ));
    }

    let mut effective = BTreeMap::<String, String>::new();
    for (key, value) in &plan.process.env {
        effective.insert(key.clone(), value.clone());
    }
    let mut removed = BTreeSet::<String>::new();
    for key in &plan.process.remove_env {
        effective.remove(key);
        removed.insert(key.clone());
    }
    for (key, value) in &plan.process.required_env {
        removed.remove(key);
        effective.insert(key.clone(), value.clone());
    }
    for (key, value) in &plan.environment_overrides {
        removed.remove(key);
        effective.insert(key.clone(), value.clone());
    }

    let mut args = vec![
        "exec".to_string(),
        "-i".to_string(),
        "-w".to_string(),
        sandbox_runtime_path(cwd),
        name.to_string(),
        "env".to_string(),
    ];
    for key in removed {
        args.push("-u".to_string());
        args.push(key);
    }
    for (key, value) in effective {
        args.push(format!("{key}={value}"));
    }
    args.extend([
        "sh".to_string(),
        "-c".to_string(),
        REMOTE_SUPERVISOR_SCRIPT.to_string(),
        "ctmcp-supervisor".to_string(),
        pidfile.to_string(),
        REMOTE_INNER_SCRIPT.to_string(),
        program,
    ]);
    args.extend(plan.process.args.clone());
    Ok(args)
}

fn host_mount_path(path: &Path) -> String {
    let display = path.to_string_lossy();
    #[cfg(windows)]
    {
        if let Some(stripped) = display.strip_prefix(r"\\?\") {
            return stripped.to_string();
        }
    }
    display.into_owned()
}

fn sandbox_runtime_path(path: &Path) -> String {
    let host = host_mount_path(path);
    #[cfg(windows)]
    {
        let bytes = host.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/')
        {
            let drive = (bytes[0] as char).to_ascii_lowercase();
            let rest = host[3..].replace('\\', "/");
            if rest.is_empty() {
                return format!("/{drive}");
            }
            return format!("/{drive}/{rest}");
        }
        host.replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        host
    }
}

fn windows_only_program(program: &Path) -> bool {
    let name = program
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name.ends_with(".exe")
        || name.ends_with(".cmd")
        || name.ends_with(".bat")
        || name.ends_with(".ps1")
}

#[cfg(windows)]
fn is_network_path(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    matches!(
        path.components().next(),
        Some(Component::Prefix(prefix))
            if matches!(prefix.kind(), Prefix::UNC(..) | Prefix::VerbatimUNC(..))
    )
}

#[cfg(not(windows))]
fn is_network_path(_path: &Path) -> bool {
    false
}

fn is_unsupported_remote_path(path: &Path) -> bool {
    is_network_path(path) && !crate::workspace::is_wsl_unc_path(path)
}

fn sbx_output_error(
    code: &'static str,
    message: &str,
    stage: &'static str,
    output: &Output,
) -> WorkspaceError {
    sbx_output_error_with_suggestion(
        code,
        message,
        stage,
        output,
        "Run sbx directly for diagnostics, then retry after resolving the reported setup/runtime error.",
    )
}

fn sbx_output_error_with_suggestion(
    code: &'static str,
    message: &str,
    stage: &'static str,
    output: &Output,
    suggestion: &str,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category: "runtime",
        retryable: false,
        details: json!({
            "backend": BACKEND_ID,
            "stage": stage,
            "fallback_allowed": false,
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "suggestion": suggestion,
        }),
    }
}

fn sbx_error(
    code: &'static str,
    message: impl Into<String>,
    stage: &'static str,
    suggestion: &'static str,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message: message.into(),
        category: "security",
        retryable: false,
        details: json!({
            "backend": BACKEND_ID,
            "stage": stage,
            "fallback_allowed": false,
            "suggestion": suggestion,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_mount_model_dedupes_and_preserves_read_only() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let shared = root.path().join("shared");
        let writable = root.path().join("writable");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&shared).expect("shared");
        std::fs::create_dir_all(&writable).expect("writable");
        let grants = vec![
            SandboxPathGrant {
                path: shared.to_string_lossy().into_owned(),
                access: SandboxPathAccess::ReadOnly,
            },
            SandboxPathGrant {
                path: writable.to_string_lossy().into_owned(),
                access: SandboxPathAccess::ReadOnly,
            },
            SandboxPathGrant {
                path: writable.to_string_lossy().into_owned(),
                access: SandboxPathAccess::Modify,
            },
        ];
        let mounts = build_mounts(&workspace, &grants).expect("mounts");
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[0].access, SandboxPathAccess::Modify);
        assert_eq!(mounts[1].access, SandboxPathAccess::ReadOnly);
        assert_eq!(mounts[2].access, SandboxPathAccess::Modify);
        assert_eq!(sandbox_name(&mounts), sandbox_name(&mounts));
    }

    #[test]
    fn file_grants_fail_closed_instead_of_broadening_to_parent_directory() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let file = root.path().join("single.txt");
        std::fs::write(&file, "secret").expect("fixture");
        let error = build_mounts(
            &workspace,
            &[SandboxPathGrant {
                path: file.to_string_lossy().into_owned(),
                access: SandboxPathAccess::ReadOnly,
            }],
        )
        .expect_err("file grant must fail closed");
        assert_eq!(error.to_error_value()["code"], "SANDBOX_SBX_PATH_INVALID");
    }

    #[test]
    fn broader_writable_mount_cannot_erase_nested_read_only_boundary() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let parent = root.path().join("external");
        let child = parent.join("readonly");
        std::fs::create_dir_all(&workspace).expect("workspace");
        std::fs::create_dir_all(&child).expect("nested external");
        let error = build_mounts(
            &workspace,
            &[
                SandboxPathGrant {
                    path: parent.to_string_lossy().into_owned(),
                    access: SandboxPathAccess::Modify,
                },
                SandboxPathGrant {
                    path: child.to_string_lossy().into_owned(),
                    access: SandboxPathAccess::ReadOnly,
                },
            ],
        )
        .expect_err("nested read-only must not be weakened by a broader writable mount");
        assert_eq!(error.to_error_value()["code"], "SANDBOX_SBX_MOUNT_OVERLAP");
    }

    #[test]
    fn listed_sandbox_parser_matches_named_entry() {
        let value = json!({"sandboxes": [{"name": "ctmcp-test"}]});
        assert!(listed_sandbox_exists(&value, "ctmcp-test"));
        assert!(!listed_sandbox_exists(&value, "ctmcp-other"));
    }

    #[cfg(windows)]
    #[test]
    fn wsl_unc_is_allowed_while_ordinary_network_shares_remain_rejected() {
        assert!(!is_unsupported_remote_path(Path::new(
            r"\\wsl.localhost\Ubuntu\home\dev"
        )));
        assert!(!is_unsupported_remote_path(Path::new(
            r"\\wsl$\Ubuntu\home\dev"
        )));
        assert!(is_unsupported_remote_path(Path::new(
            r"\\server\share\folder"
        )));
        assert!(is_unsupported_remote_path(Path::new(
            r"\\?\UNC\server\share\folder"
        )));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_split_host_mount_and_linux_runtime_forms() {
        let path = Path::new(r"\\?\C:\Users\Agent\Project");
        assert_eq!(host_mount_path(path), r"C:\Users\Agent\Project");
        assert_eq!(sandbox_runtime_path(path), "/c/Users/Agent/Project");
    }

    #[cfg(windows)]
    struct LiveSandboxGuard {
        cli: PathBuf,
        name: String,
    }

    #[cfg(windows)]
    impl Drop for LiveSandboxGuard {
        fn drop(&mut self) {
            let _ = Command::new(&self.cli)
                .args(["rm", "-f", &self.name])
                .output();
        }
    }

    #[cfg(windows)]
    async fn live_output(
        provider: &DockerSbxPreparedSandbox,
        cwd: &Path,
        program: &str,
        args: Vec<String>,
        env: Vec<(String, String)>,
        stdin: Option<&[u8]>,
    ) -> crate::tools::process_child::ProcessChildOutput {
        use tokio::io::AsyncWriteExt;

        let plan = provider
            .prepare_command(
                SandboxCommand::new(PathBuf::from(program), args, cwd.to_path_buf()),
                env,
                Vec::new(),
            )
            .expect("live process plan");
        let mut child = provider
            .launch_prepared_process(plan)
            .expect("live process launch");
        if let Some(input) = stdin {
            let mut writer = child.take_stdin().expect("live stdin");
            writer.write_all(input).await.expect("write live stdin");
            writer.shutdown().await.expect("shutdown live stdin");
        }
        child.wait_with_output().await.expect("live process output")
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn live_docker_sbx_provider_when_explicitly_enabled() {
        use std::sync::Arc;
        use std::time::Duration;

        if std::env::var("CTMCP_TEST_DOCKER_SBX").as_deref() != Ok("1") {
            return;
        }

        let cli = discover_sbx_program().expect("sbx CLI");
        let root = tempfile::tempdir().expect("live root");
        let workspace = root.path().join("workspace");
        let readonly = root.path().join("readonly");
        let writable = root.path().join("writable");
        let hidden = root.path().join("hidden");
        for path in [&workspace, &readonly, &writable, &hidden] {
            std::fs::create_dir_all(path).expect("live fixture directory");
        }
        std::fs::write(workspace.join("marker.txt"), "workspace-marker\n").expect("marker");
        std::fs::write(readonly.join("readonly.txt"), "readonly-marker\n").expect("readonly");
        std::fs::write(hidden.join("secret.txt"), "hidden-secret\n").expect("hidden");

        let mounts = build_mounts(
            &workspace,
            &[
                SandboxPathGrant {
                    path: readonly.to_string_lossy().into_owned(),
                    access: SandboxPathAccess::ReadOnly,
                },
                SandboxPathGrant {
                    path: writable.to_string_lossy().into_owned(),
                    access: SandboxPathAccess::Modify,
                },
            ],
        )
        .expect("live mounts");
        let name = format!("ctmcp-live-{}", Uuid::new_v4().simple());
        ensure_sandbox(&cli, &name, &mounts).expect("create live sandbox");
        let _guard = LiveSandboxGuard {
            cli: cli.clone(),
            name: name.clone(),
        };
        ensure_remote_supervisor(&cli, &name).expect("live supervisor");
        let provider = DockerSbxPreparedSandbox {
            cli,
            sandbox_name: name,
            mounts,
        };

        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec![
                "-lc".into(),
                "printf '%s|' \"$CTMCP_LIVE\"; cat; printf workspace-write > live.txt".into(),
            ],
            vec![("CTMCP_LIVE".into(), "ok".into())],
            Some(b"stdin-ok"),
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"ok|stdin-ok");
        assert_eq!(
            std::fs::read_to_string(workspace.join("live.txt")).expect("workspace write"),
            "workspace-write"
        );

        let readonly_vm = sandbox_runtime_path(&readonly);
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec!["-lc".into(), format!("cat '{readonly_vm}/readonly.txt'")],
            Vec::new(),
            None,
        )
        .await;
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "readonly-marker"
        );
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec![
                "-lc".into(),
                format!("printf blocked > '{readonly_vm}/blocked.txt'"),
            ],
            Vec::new(),
            None,
        )
        .await;
        assert!(!output.status.success());
        assert!(!readonly.join("blocked.txt").exists());

        let writable_vm = sandbox_runtime_path(&writable);
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec![
                "-lc".into(),
                format!("printf allowed > '{writable_vm}/allowed.txt'"),
            ],
            Vec::new(),
            None,
        )
        .await;
        assert!(output.status.success());
        assert_eq!(
            std::fs::read_to_string(writable.join("allowed.txt")).expect("writable write"),
            "allowed"
        );

        let hidden_vm = sandbox_runtime_path(&hidden);
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec!["-lc".into(), format!("test ! -e '{hidden_vm}/secret.txt'")],
            Vec::new(),
            None,
        )
        .await;
        assert!(output.status.success());

        let cancel_marker = workspace.join("cancel-leak.txt");
        let plan = provider
            .prepare_command(
                SandboxCommand::new(
                    PathBuf::from("sh"),
                    vec![
                        "-lc".into(),
                        "sleep 5; printf leaked > cancel-leak.txt".into(),
                    ],
                    workspace.clone(),
                ),
                Vec::new(),
                Vec::new(),
            )
            .expect("cancel process plan");
        let child = provider
            .launch_prepared_process(plan)
            .expect("cancel process launch");
        let session = Arc::new(
            crate::tools::session::ExecSession::new_with_mode_and_checks(child, false, false),
        );
        session.spawn_readers().await;
        session.spawn_exit_waiter();
        tokio::time::sleep(Duration::from_secs(1)).await;
        session.kill_and_wait().await;
        tokio::time::sleep(Duration::from_secs(6)).await;
        assert!(
            !cancel_marker.exists(),
            "remote command survived session cancellation"
        );
    }

    #[test]
    fn exec_args_keep_environment_changes_inside_sandbox() {
        let root = tempfile::tempdir().expect("root");
        let plan = SandboxProcessPlan {
            backend_id: BACKEND_ID.into(),
            process: ProcessLaunchSpec {
                program: PathBuf::from("python"),
                args: vec!["--version".into()],
                cwd: Some(root.path().to_path_buf()),
                env: vec![("KEEP".into(), "1".into()), ("DROP".into(), "old".into())],
                remove_env: vec!["DROP".into()],
                required_env: vec![("REQ".into(), "2".into())],
                windows_raw_arg: None,
                using_wsl: false,
            },
            environment_overrides: BTreeMap::from([("BACKEND".into(), "3".into())]),
            state: None,
        };
        let args = build_exec_args("ctmcp-test", &plan, "/tmp/test.pid").expect("exec args");
        assert_eq!(
            &args[..6],
            [
                "exec",
                "-i",
                "-w",
                &sandbox_runtime_path(root.path()),
                "ctmcp-test",
                "env"
            ]
        );
        assert!(args.windows(2).any(|pair| pair == ["-u", "DROP"]));
        assert!(args.contains(&"KEEP=1".to_string()));
        assert!(args.contains(&"REQ=2".to_string()));
        assert!(args.contains(&"BACKEND=3".to_string()));
        assert!(args.contains(&REMOTE_SUPERVISOR_SCRIPT.to_string()));
        assert!(args.contains(&REMOTE_INNER_SCRIPT.to_string()));
        assert!(args.contains(&"/tmp/test.pid".to_string()));
        assert_eq!(&args[args.len() - 2..], ["python", "--version"]);
    }
}
