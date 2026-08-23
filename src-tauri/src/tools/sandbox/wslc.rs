use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::process::Command as TokioCommand;
use uuid::Uuid;

use crate::platform::platform;
use crate::tools::process_child::{ProcessChild, ProcessKillHook};
use crate::tools::process_spec::ProcessLaunchSpec;
use crate::tools::workspace::{Workspace, WorkspaceError, WorkspaceResult};
use crate::workspace::{SandboxPathAccess, SandboxPathGrant};

use super::{PreparedSandbox, SandboxCommand, SandboxProcessPlan};

mod session;

const BACKEND_ID: &str = "wslc";
const IMAGE_OPTION_ID: &str = "wslc.image";
const NETWORK_OPTION_ID: &str = "wslc.network";
const MAX_SESSION_MOUNTS: usize = 15;
pub(super) const DEFAULT_IMAGE: &str = "ubuntu:24.04";
pub(super) const DEFAULT_NETWORK: &str = "none";

#[derive(Debug, Clone, PartialEq, Eq)]
struct WslcMount {
    host: PathBuf,
    container: String,
    access: SandboxPathAccess,
}

pub(crate) fn discover_wslc_program() -> Option<PathBuf> {
    if let Ok(path) = which::which("wslc") {
        return Some(path);
    }

    #[cfg(windows)]
    {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            let candidate = PathBuf::from(program_files).join("WSL").join("wslc.exe");
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
    options: &BTreeMap<String, String>,
) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
    let storage = managed_storage_path(workspace)?;
    prepare_with_storage(workspace, grants, options, storage)
}

fn prepare_with_storage(
    workspace: &Workspace,
    grants: &[SandboxPathGrant],
    options: &BTreeMap<String, String>,
    storage: PathBuf,
) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
    let cli = discover_wslc_program().ok_or_else(|| {
        wslc_error(
            "SANDBOX_WSLC_UNAVAILABLE",
            "Microsoft WSL Containers CLI (wslc.exe) was not found.",
            "discovery",
            "Update WSL to a release that includes WSL Containers and ensure wslc.exe is available.",
        )
    })?;
    ensure_cli_ready(&cli)?;
    let image = selected_image(options)?;
    let network = selected_network(options)?;
    let mounts = build_mounts(workspace.root(), grants)?;
    let session = session::acquire(&cli, &storage)?;
    ensure_image(&cli, session.as_ref(), &image)?;

    Ok(Box::new(WslcPreparedSandbox {
        cli,
        image,
        network,
        mounts,
        session,
    }))
}

struct WslcPreparedSandbox {
    cli: PathBuf,
    image: String,
    network: String,
    mounts: Vec<WslcMount>,
    session: Arc<session::WslcSessionCoordinator>,
}

impl PreparedSandbox for WslcPreparedSandbox {
    fn backend_id(&self) -> &str {
        BACKEND_ID
    }

    fn normalize_logical_command(
        &self,
        mut command: SandboxCommand,
    ) -> WorkspaceResult<SandboxCommand> {
        let cwd = canonical_existing_directory(&command.cwd, "command working directory")?;
        if self.container_path_for_host(&cwd).is_none() {
            return Err(wslc_error(
                "SANDBOX_WSLC_PATH_UNMOUNTED",
                format!(
                    "Command working directory is not mounted in the WSLC container: {}",
                    command.cwd.display()
                ),
                "prepare_command",
                "Run inside the workspace or add the directory as an explicit sandbox path grant.",
            ));
        }
        command.cwd = cwd;

        if windows_only_program(&command.executable) {
            return Err(wslc_error(
                "SANDBOX_WSLC_COMMAND_UNSUPPORTED",
                format!(
                    "Windows host executable cannot run in a WSLC Linux container: {}",
                    command.executable.display()
                ),
                "prepare_command",
                "Use a Linux-side command name provided by the selected container image.",
            ));
        }

        if command.executable.is_absolute() {
            let executable = command.executable.canonicalize().map_err(|error| {
                wslc_error(
                    "SANDBOX_WSLC_COMMAND_UNAVAILABLE",
                    format!(
                        "Sandbox command path is unavailable: {}: {error}",
                        command.executable.display()
                    ),
                    "prepare_command",
                    "Use a command installed in the image or a mounted workspace-local executable.",
                )
            })?;
            if !executable.is_file() || self.container_path_for_host(&executable).is_none() {
                return Err(wslc_error(
                    "SANDBOX_WSLC_COMMAND_UNMOUNTED",
                    format!(
                        "Sandbox command path is outside mounted workspaces: {}",
                        command.executable.display()
                    ),
                    "prepare_command",
                    "Use a command installed in the image or a mounted workspace-local executable.",
                ));
            }
            command.executable = executable;
        } else if command.executable.components().count() > 1 {
            return Err(wslc_error(
                "SANDBOX_WSLC_COMMAND_UNAVAILABLE",
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
            return Err(wslc_error(
                "SANDBOX_WSLC_PROCESS_UNSUPPORTED",
                "Windows/WSL host process normalization cannot be forwarded into a WSLC Linux container.",
                "prepare_process",
                "Use a Linux-side command supported by the selected container image.",
            ));
        }
        if let Some(cwd) = process.cwd.as_ref() {
            let canonical = canonical_existing_directory(cwd, "command working directory")?;
            if self.container_path_for_host(&canonical).is_none() {
                return Err(wslc_error(
                    "SANDBOX_WSLC_PATH_UNMOUNTED",
                    format!(
                        "Command working directory is not mounted in the WSLC container: {}",
                        cwd.display()
                    ),
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
            return Err(wslc_error(
                "SANDBOX_PROCESS_PLAN_INVALID",
                format!(
                    "Prepared process backend '{}' does not match '{}'.",
                    plan.backend_id, BACKEND_ID
                ),
                "launch",
                "Rebuild the process plan with the selected sandbox backend.",
            ));
        }

        let name = format!("ctmcp-wslc-{}", Uuid::new_v4().simple());
        let args = build_run_args(&name, &self.image, &self.network, &self.mounts, &plan)?;
        let reservation = self.session.reserve_mounts(self.mounts.len())?;
        let session_name = reservation.name().to_string();
        let mut command = TokioCommand::new(&self.cli);
        command
            .arg("--session")
            .arg(&session_name)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = command.spawn().map_err(|error| {
            wslc_error(
                "SANDBOX_WSLC_RUN_FAILED",
                format!("Failed to start wslc run: {error}"),
                "launch",
                "Verify WSL Containers is healthy and the configured image can run.",
            )
        })?;
        drop(reservation);

        let cleanup = WslcContainerGuard {
            cli: self.cli.clone(),
            session_name: session_name.clone(),
            name: name.clone(),
        };
        let kill_hook = container_kill_hook(self.cli.clone(), session_name, name);
        Ok(ProcessChild::from_tokio(child)
            .with_process_tree_contained(true)
            .with_kill_hook(kill_hook)
            .with_backend_lifetime(cleanup))
    }
}

impl WslcPreparedSandbox {
    fn container_path_for_host(&self, path: &Path) -> Option<String> {
        container_path_for_host(&self.mounts, path)
    }
}

struct WslcContainerGuard {
    cli: PathBuf,
    session_name: String,
    name: String,
}

impl Drop for WslcContainerGuard {
    fn drop(&mut self) {
        let _ = Command::new(&self.cli)
            .args(["--session", &self.session_name, "remove", "-f", &self.name])
            .output();
    }
}

fn selected_image(options: &BTreeMap<String, String>) -> WorkspaceResult<String> {
    let image = options
        .get(IMAGE_OPTION_ID)
        .map(String::as_str)
        .unwrap_or(DEFAULT_IMAGE)
        .trim();
    let image = if image.is_empty() {
        DEFAULT_IMAGE
    } else {
        image
    };
    if image.chars().any(char::is_whitespace) || image.chars().any(char::is_control) {
        return Err(wslc_error(
            "SANDBOX_WSLC_IMAGE_INVALID",
            "WSLC container image contains whitespace or control characters.",
            "image",
            "Use an OCI image reference such as ubuntu:24.04 or registry.example.com/team/dev:tag.",
        ));
    }
    Ok(image.to_string())
}

fn selected_network(options: &BTreeMap<String, String>) -> WorkspaceResult<String> {
    let network = options
        .get(NETWORK_OPTION_ID)
        .map(String::as_str)
        .unwrap_or(DEFAULT_NETWORK)
        .trim();
    let network = if network.is_empty() {
        DEFAULT_NETWORK
    } else {
        network
    };
    if !network
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '.'))
    {
        return Err(wslc_error(
            "SANDBOX_WSLC_NETWORK_INVALID",
            "WSLC network name contains unsupported characters.",
            "network",
            "Use 'none', 'bridge', or a simple WSLC network name containing letters, digits, '.', '_' or '-'.",
        ));
    }
    Ok(network.to_string())
}

fn ensure_cli_ready(cli: &Path) -> WorkspaceResult<()> {
    let output = run_wslc(cli, ["version"])?;
    if output.status.success() {
        return Ok(());
    }
    Err(wslc_output_error(
        "SANDBOX_WSLC_UNAVAILABLE",
        "wslc version failed.",
        "discovery",
        &output,
    ))
}

fn ensure_image(
    _cli: &Path,
    session: &session::WslcSessionCoordinator,
    image: &str,
) -> WorkspaceResult<()> {
    let inspected = session.run(&["image", "inspect", image])?;
    if inspected.status.success() {
        return Ok(());
    }
    let pulled = session.run(&["pull", image])?;
    if pulled.status.success() {
        return Ok(());
    }
    Err(wslc_output_error_with_suggestion(
        "SANDBOX_WSLC_IMAGE_UNAVAILABLE",
        "WSLC could not inspect or pull the configured container image.",
        "image",
        &pulled,
        "Verify the image reference and registry/network access, then retry.",
    ))
}

fn canonical_existing_directory(path: &Path, label: &str) -> WorkspaceResult<PathBuf> {
    if is_unsupported_remote_path(path) {
        return Err(wslc_error(
            "SANDBOX_WSLC_PATH_UNSUPPORTED",
            format!(
                "Network-backed host paths are not supported: {}",
                path.display()
            ),
            "mounts",
            "Use a local filesystem directory or a WSL folder path (\\\\wsl.localhost\\<distro>\\...).",
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        wslc_error(
            "SANDBOX_WSLC_PATH_INVALID",
            format!("{label} is unavailable: {}: {error}", path.display()),
            "mounts",
            "Use an existing local directory.",
        )
    })?;
    if !canonical.is_dir() {
        return Err(wslc_error(
            "SANDBOX_WSLC_PATH_INVALID",
            format!("{label} must be a directory: {}", path.display()),
            "mounts",
            "WSLC sandbox path grants must reference directories.",
        ));
    }
    if is_unsupported_remote_path(&canonical) {
        return Err(wslc_error(
            "SANDBOX_WSLC_PATH_UNSUPPORTED",
            format!(
                "Network-backed host paths are not supported: {}",
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
) -> WorkspaceResult<Vec<WslcMount>> {
    let workspace = canonical_existing_directory(workspace_root, "workspace root")?;
    let mut external = BTreeMap::<PathBuf, SandboxPathAccess>::new();
    for grant in grants {
        if grant.path.trim().is_empty() {
            continue;
        }
        let raw = PathBuf::from(grant.path.trim());
        let path = canonical_existing_directory(&raw, "external sandbox path")?;
        if path.starts_with(&workspace) {
            continue;
        }
        if workspace.starts_with(&path) {
            return Err(wslc_error(
                "SANDBOX_WSLC_MOUNT_OVERLAP",
                format!(
                    "External sandbox path contains the primary workspace: {}",
                    raw.display()
                ),
                "mounts",
                "Grant a sibling directory instead of an ancestor of the workspace.",
            ));
        }
        external
            .entry(path)
            .and_modify(|access| {
                if grant.access == SandboxPathAccess::Modify {
                    *access = SandboxPathAccess::Modify;
                }
            })
            .or_insert(grant.access);
    }

    let entries = external.iter().collect::<Vec<_>>();
    for (parent_path, parent_access) in &entries {
        if **parent_access != SandboxPathAccess::Modify {
            continue;
        }
        for (child_path, child_access) in &entries {
            if parent_path != child_path
                && child_path.starts_with(parent_path)
                && **child_access == SandboxPathAccess::ReadOnly
            {
                return Err(wslc_error(
                    "SANDBOX_WSLC_MOUNT_OVERLAP",
                    format!(
                        "Writable external path contains a read-only grant: {} contains {}",
                        parent_path.display(),
                        child_path.display()
                    ),
                    "mounts",
                    "Remove the broader writable grant or make the nested grant writable too.",
                ));
            }
        }
    }

    let mut mounts = vec![WslcMount {
        host: workspace,
        container: "/workspace".into(),
        access: SandboxPathAccess::Modify,
    }];
    mounts.extend(
        external
            .into_iter()
            .enumerate()
            .map(|(index, (host, access))| WslcMount {
                host,
                container: format!("/ctmcp/grants/{index}"),
                access,
            }),
    );
    Ok(mounts)
}

fn build_run_args(
    name: &str,
    image: &str,
    network: &str,
    mounts: &[WslcMount],
    plan: &SandboxProcessPlan,
) -> WorkspaceResult<Vec<String>> {
    let cwd = plan.process.cwd.as_ref().ok_or_else(|| {
        wslc_error(
            "SANDBOX_PROCESS_PLAN_INVALID",
            "WSLC process plan requires a working directory.",
            "launch",
            "Provide a workspace working directory.",
        )
    })?;
    let container_cwd = container_path_for_host(mounts, cwd).ok_or_else(|| {
        wslc_error(
            "SANDBOX_WSLC_PATH_UNMOUNTED",
            format!(
                "Command working directory is not mounted: {}",
                cwd.display()
            ),
            "launch",
            "Use a working directory inside the workspace or an explicit path grant.",
        )
    })?;
    let program = if plan.process.program.is_absolute() {
        container_path_for_host(mounts, &plan.process.program).ok_or_else(|| {
            wslc_error(
                "SANDBOX_WSLC_COMMAND_UNMOUNTED",
                format!(
                    "Sandbox command path is not mounted: {}",
                    plan.process.program.display()
                ),
                "launch",
                "Use a command installed in the image or a mounted workspace-local executable.",
            )
        })?
    } else {
        plan.process.program.to_string_lossy().into_owned()
    };
    if program.trim().is_empty() {
        return Err(wslc_error(
            "SANDBOX_PROCESS_PLAN_INVALID",
            "WSLC process plan has an empty command.",
            "launch",
            "Provide a command to execute inside the container.",
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
        "run".to_string(),
        "--rm".to_string(),
        "-i".to_string(),
        "--name".to_string(),
        name.to_string(),
        "--network".to_string(),
        network.to_string(),
    ];
    for mount in mounts {
        args.push("-v".into());
        let mut value = format!("{}:{}", host_mount_path(&mount.host), mount.container);
        if mount.access == SandboxPathAccess::ReadOnly {
            value.push_str(":ro");
        }
        args.push(value);
    }
    args.extend(["-w".into(), container_cwd]);
    for (key, value) in effective {
        args.push("-e".into());
        args.push(format!("{key}={value}"));
    }
    args.push(image.to_string());
    if !removed.is_empty() {
        args.push("env".into());
        for key in removed {
            args.push("-u".into());
            args.push(key);
        }
    }
    args.push(program);
    args.extend(plan.process.args.clone());
    Ok(args)
}

fn container_path_for_host(mounts: &[WslcMount], path: &Path) -> Option<String> {
    let comparable_path = comparable_host_path(path);
    let mount = mounts
        .iter()
        .filter(|mount| comparable_path.starts_with(comparable_host_path(&mount.host)))
        .max_by_key(|mount| mount.host.components().count())?;
    let comparable_mount = comparable_host_path(&mount.host);
    let relative = comparable_path.strip_prefix(&comparable_mount).ok()?;
    if relative.as_os_str().is_empty() {
        return Some(mount.container.clone());
    }
    let suffix = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/");
    Some(format!(
        "{}/{suffix}",
        mount.container.trim_end_matches('/')
    ))
}

fn comparable_host_path(path: &Path) -> PathBuf {
    PathBuf::from(host_mount_path(path))
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

fn managed_storage_path(workspace: &Workspace) -> WorkspaceResult<PathBuf> {
    let canonical = canonical_existing_directory(workspace.root(), "workspace root")?;
    let mut identity = host_mount_path(&canonical);
    #[cfg(windows)]
    {
        identity.make_ascii_lowercase();
    }
    let mut hasher = Sha256::new();
    hasher.update(identity.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    let config_root = platform().app_config_dir().map_err(|error| {
        wslc_error(
            "SANDBOX_WSLC_SESSION_PREPARE_FAILED",
            format!("Failed to resolve WSLC managed session storage: {error}"),
            "session_storage",
            "Verify the application data directory is available and retry.",
        )
    })?;
    Ok(config_root
        .join("sandbox")
        .join("wslc")
        .join("sessions")
        .join(&digest[..32]))
}

fn container_kill_hook(cli: PathBuf, session_name: String, name: String) -> ProcessKillHook {
    Arc::new(move || cancel_container(&cli, &session_name, &name))
}

fn cancel_container(cli: &Path, session_name: &str, name: &str) -> io::Result<()> {
    let mut last_error = String::new();
    for _ in 0..20 {
        match Command::new(cli)
            .args(["--session", session_name, "remove", "-f", name])
            .output()
        {
            Ok(output) if output.status.success() => return Ok(()),
            Ok(output) => {
                last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            }
            Err(error) => return Err(error),
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::other(format!(
        "wslc container cancellation failed for {name}: {last_error}"
    )))
}

fn run_wslc<'a>(cli: &Path, args: impl IntoIterator<Item = &'a str>) -> WorkspaceResult<Output> {
    Command::new(cli).args(args).output().map_err(|error| {
        wslc_error(
            "SANDBOX_WSLC_UNAVAILABLE",
            format!("Failed to execute wslc CLI: {error}"),
            "cli",
            "Update or repair WSL Containers, then retry.",
        )
    })
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

fn wslc_output_error(
    code: &'static str,
    message: &str,
    stage: &'static str,
    output: &Output,
) -> WorkspaceError {
    wslc_output_error_with_suggestion(
        code,
        message,
        stage,
        output,
        "Run wslc directly for diagnostics, resolve the reported runtime error, then retry.",
    )
}

fn wslc_output_error_with_suggestion(
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

fn wslc_error(
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
    }

    #[test]
    fn default_image_is_explicit_and_versioned() {
        assert_eq!(DEFAULT_IMAGE, "ubuntu:24.04");
        assert_eq!(
            selected_image(&BTreeMap::new()).expect("default image"),
            DEFAULT_IMAGE
        );
        assert_eq!(
            selected_network(&BTreeMap::new()).expect("default network"),
            "none"
        );
    }

    #[test]
    fn mount_model_assigns_private_container_paths_and_preserves_access() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let readonly = root.path().join("readonly");
        let writable = root.path().join("writable");
        for path in [&workspace, &readonly, &writable] {
            std::fs::create_dir_all(path).expect("fixture");
        }
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
        .expect("mounts");
        assert_eq!(mounts[0].container, "/workspace");
        assert_eq!(mounts[0].access, SandboxPathAccess::Modify);
        assert!(mounts
            .iter()
            .any(|mount| mount.access == SandboxPathAccess::ReadOnly));
        assert_eq!(
            container_path_for_host(&mounts, &workspace.join("src")),
            Some("/workspace/src".into())
        );
    }

    #[test]
    fn run_args_create_one_ephemeral_container_with_explicit_mounts() {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        let mounts = build_mounts(&workspace, &[]).expect("mounts");
        let plan = SandboxProcessPlan {
            backend_id: BACKEND_ID.into(),
            process: ProcessLaunchSpec {
                program: PathBuf::from("python"),
                args: vec!["--version".into()],
                cwd: Some(workspace.clone()),
                env: vec![("KEEP".into(), "1".into())],
                remove_env: vec!["DROP".into()],
                required_env: Vec::new(),
                windows_raw_arg: None,
                using_wsl: false,
            },
            environment_overrides: BTreeMap::new(),
            state: None,
        };
        let args = build_run_args("ctmcp-test", DEFAULT_IMAGE, DEFAULT_NETWORK, &mounts, &plan)
            .expect("args");
        assert_eq!(&args[..5], ["run", "--rm", "-i", "--name", "ctmcp-test"]);
        assert!(args.windows(2).any(|pair| pair == ["--network", "none"]));
        assert!(args.contains(&"-v".to_string()));
        assert!(args.contains(&"/workspace".to_string()));
        assert!(args.windows(2).any(|pair| pair == ["-u", "DROP"]));
        assert!(args.windows(2).any(|pair| pair == ["-e", "KEEP=1"]));
        assert_eq!(&args[args.len() - 2..], ["python", "--version"]);
    }

    #[cfg(windows)]
    async fn live_output(
        provider: &WslcPreparedSandbox,
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
        child.wait_with_output().await.expect("live output")
    }

    #[cfg(windows)]
    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn live_wslc_provider_when_explicitly_enabled() {
        use std::sync::Arc;

        if std::env::var("CTMCP_TEST_WSLC").as_deref() != Ok("1") {
            return;
        }

        let cli = discover_wslc_program().expect("wslc CLI");
        let root = tempfile::tempdir().expect("live root");
        let workspace = root.path().join("workspace");
        let readonly = root.path().join("readonly");
        let writable = root.path().join("writable");
        let hidden = root.path().join("hidden");
        for path in [&workspace, &readonly, &writable, &hidden] {
            std::fs::create_dir_all(path).expect("live fixture");
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
        .expect("mounts");
        let image = std::env::var("CTMCP_TEST_WSLC_IMAGE").unwrap_or_else(|_| "alpine:3.20".into());
        ensure_cli_ready(&cli).expect("wslc ready");
        let managed_session = session::acquire(&cli, &root.path().join("wslc-session-storage"))
            .expect("live managed session");
        ensure_image(&cli, managed_session.as_ref(), &image).expect("live image");
        let provider = WslcPreparedSandbox {
            cli,
            image,
            network: DEFAULT_NETWORK.into(),
            mounts,
            session: managed_session,
        };

        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec![
                "-c".into(),
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
            std::fs::read_to_string(workspace.join("live.txt")).unwrap(),
            "workspace-write"
        );

        let readonly_vm = provider
            .container_path_for_host(&readonly)
            .expect("readonly path");
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec![
                "-c".into(),
                format!("printf blocked > '{readonly_vm}/blocked.txt'"),
            ],
            Vec::new(),
            None,
        )
        .await;
        assert!(!output.status.success());
        assert!(!readonly.join("blocked.txt").exists());

        let writable_vm = provider
            .container_path_for_host(&writable)
            .expect("writable path");
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec![
                "-c".into(),
                format!("printf allowed > '{writable_vm}/allowed.txt'"),
            ],
            Vec::new(),
            None,
        )
        .await;
        assert!(output.status.success());
        assert_eq!(
            std::fs::read_to_string(writable.join("allowed.txt")).unwrap(),
            "allowed"
        );

        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec!["-c".into(), "test ! -e /ctmcp/hidden/secret.txt".into()],
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
                        "-c".into(),
                        "sleep 5; printf leaked > cancel-leak.txt".into(),
                    ],
                    workspace.clone(),
                ),
                Vec::new(),
                Vec::new(),
            )
            .expect("cancel plan");
        let child = provider
            .launch_prepared_process(plan)
            .expect("cancel child");
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
            "WSLC command survived container cancellation"
        );

        // Five commands above consume exactly 15 volume attachments (workspace +
        // two grants each) on WSLC 2.9.x. The sixth command must transparently
        // reopen the same managed storage instead of hitting 0x8007000e.
        let output = live_output(
            &provider,
            &workspace,
            "sh",
            vec!["-c".into(), "printf session-rotated".into()],
            Vec::new(),
            None,
        )
        .await;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(output.stdout, b"session-rotated");
    }
}
