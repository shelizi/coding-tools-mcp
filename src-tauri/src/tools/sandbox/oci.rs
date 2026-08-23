use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::json;
use tokio::process::Command as TokioCommand;
use uuid::Uuid;

use crate::tools::process_child::{ProcessChild, ProcessKillHook};
use crate::tools::process_spec::ProcessLaunchSpec;
use crate::tools::workspace::{Workspace, WorkspaceError, WorkspaceResult};
use crate::workspace::{SandboxPathAccess, SandboxPathGrant};

use super::{PreparedSandbox, SandboxCommand, SandboxProcessPlan};

pub(super) const DOCKER_BACKEND_ID: &str = "docker";
pub(super) const PODMAN_BACKEND_ID: &str = "podman";
pub(super) const DEFAULT_IMAGE: &str = "ubuntu:24.04";
pub(super) const DEFAULT_NETWORK: &str = "none";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OciRuntime {
    Docker,
    Podman,
}

impl OciRuntime {
    fn id(self) -> &'static str {
        match self {
            Self::Docker => DOCKER_BACKEND_ID,
            Self::Podman => PODMAN_BACKEND_ID,
        }
    }

    fn cli_name(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }

    fn image_option_id(self) -> &'static str {
        match self {
            Self::Docker => "docker.image",
            Self::Podman => "podman.image",
        }
    }

    fn network_option_id(self) -> &'static str {
        match self {
            Self::Docker => "docker.network",
            Self::Podman => "podman.network",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OciMount {
    host: PathBuf,
    container: String,
    access: SandboxPathAccess,
}

pub(crate) fn discover_docker_program() -> Option<PathBuf> {
    discover_cli(OciRuntime::Docker)
}

pub(crate) fn discover_podman_program() -> Option<PathBuf> {
    discover_cli(OciRuntime::Podman)
}

fn discover_cli(runtime: OciRuntime) -> Option<PathBuf> {
    if let Ok(path) = which::which(runtime.cli_name()) {
        if path.is_file() {
            return Some(path);
        }
    }

    #[cfg(windows)]
    {
        for candidate in well_known_windows_cli_paths(runtime) {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    None
}

#[cfg(windows)]
fn well_known_windows_cli_paths(runtime: OciRuntime) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    match runtime {
        OciRuntime::Docker => {
            if let Some(program_files) = std::env::var_os("ProgramFiles") {
                paths.push(
                    PathBuf::from(program_files)
                        .join("Docker")
                        .join("Docker")
                        .join("resources")
                        .join("bin")
                        .join("docker.exe"),
                );
            }
        }
        OciRuntime::Podman => {
            if let Some(program_files) = std::env::var_os("ProgramFiles") {
                paths.push(
                    PathBuf::from(program_files)
                        .join("RedHat")
                        .join("Podman")
                        .join("podman.exe"),
                );
            }
        }
    }
    paths
}

pub(super) fn prepare(
    runtime: OciRuntime,
    workspace: &Workspace,
    grants: &[SandboxPathGrant],
    options: &BTreeMap<String, String>,
) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
    let cli = discover_cli(runtime).ok_or_else(|| {
        oci_error(
            runtime,
            "SANDBOX_OCI_UNAVAILABLE",
            format!(
                "{} CLI ({}) was not found.",
                runtime_label(runtime),
                runtime.cli_name()
            ),
            "discovery",
            "Install the engine from Software management or the official package, then retry.",
        )
    })?;
    ensure_engine_ready(runtime, &cli)?;
    let image = selected_image(runtime, options)?;
    let network = selected_network(runtime, options)?;
    let mounts = build_mounts(runtime, workspace.root(), grants)?;
    ensure_image(runtime, &cli, &image)?;

    Ok(Box::new(OciPreparedSandbox {
        runtime,
        cli,
        image,
        network,
        mounts,
    }))
}

struct OciPreparedSandbox {
    runtime: OciRuntime,
    cli: PathBuf,
    image: String,
    network: String,
    mounts: Vec<OciMount>,
}

impl PreparedSandbox for OciPreparedSandbox {
    fn backend_id(&self) -> &str {
        self.runtime.id()
    }

    fn normalize_logical_command(
        &self,
        mut command: SandboxCommand,
    ) -> WorkspaceResult<SandboxCommand> {
        let cwd =
            canonical_existing_directory(self.runtime, &command.cwd, "command working directory")?;
        if self.container_path_for_host(&cwd).is_none() {
            return Err(oci_error(
                self.runtime,
                "SANDBOX_OCI_PATH_UNMOUNTED",
                format!(
                    "Command working directory is not mounted in the {} container: {}",
                    runtime_label(self.runtime),
                    command.cwd.display()
                ),
                "prepare_command",
                "Run inside the workspace or add the directory as an explicit sandbox path grant.",
            ));
        }
        command.cwd = cwd;

        if windows_only_program(&command.executable) {
            return Err(oci_error(
                self.runtime,
                "SANDBOX_OCI_COMMAND_UNSUPPORTED",
                format!(
                    "Windows host executable cannot run in a {} Linux container: {}",
                    runtime_label(self.runtime),
                    command.executable.display()
                ),
                "prepare_command",
                "Use a Linux-side command name provided by the selected container image.",
            ));
        }

        if command.executable.is_absolute() {
            let executable = command.executable.canonicalize().map_err(|error| {
                oci_error(
                    self.runtime,
                    "SANDBOX_OCI_COMMAND_UNAVAILABLE",
                    format!(
                        "Sandbox command path is unavailable: {}: {error}",
                        command.executable.display()
                    ),
                    "prepare_command",
                    "Use a command installed in the image or a mounted workspace-local executable.",
                )
            })?;
            if !executable.is_file() || self.container_path_for_host(&executable).is_none() {
                return Err(oci_error(
                    self.runtime,
                    "SANDBOX_OCI_COMMAND_UNMOUNTED",
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
            return Err(oci_error(
                self.runtime,
                "SANDBOX_OCI_COMMAND_UNAVAILABLE",
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
            return Err(oci_error(
                self.runtime,
                "SANDBOX_OCI_PROCESS_UNSUPPORTED",
                format!(
                    "Windows/WSL host process normalization cannot be forwarded into a {} Linux container.",
                    runtime_label(self.runtime)
                ),
                "prepare_process",
                "Use a Linux-side command supported by the selected container image.",
            ));
        }
        if let Some(cwd) = process.cwd.as_ref() {
            let canonical =
                canonical_existing_directory(self.runtime, cwd, "command working directory")?;
            if self.container_path_for_host(&canonical).is_none() {
                return Err(oci_error(
                    self.runtime,
                    "SANDBOX_OCI_PATH_UNMOUNTED",
                    format!(
                        "Command working directory is not mounted in the {} container: {}",
                        runtime_label(self.runtime),
                        cwd.display()
                    ),
                    "prepare_process",
                    "Run inside the workspace or add the directory as an explicit sandbox path grant.",
                ));
            }
            process.cwd = Some(canonical);
        }
        Ok(SandboxProcessPlan {
            backend_id: self.runtime.id().into(),
            process,
            environment_overrides: BTreeMap::new(),
            state: None,
        })
    }

    fn launch_prepared_process(&self, plan: SandboxProcessPlan) -> WorkspaceResult<ProcessChild> {
        if plan.backend_id != self.runtime.id() {
            return Err(oci_error(
                self.runtime,
                "SANDBOX_PROCESS_PLAN_INVALID",
                format!(
                    "Prepared process backend '{}' does not match '{}'.",
                    plan.backend_id,
                    self.runtime.id()
                ),
                "launch",
                "Rebuild the process plan with the selected sandbox backend.",
            ));
        }

        let name = format!("ctmcp-{}-{}", self.runtime.id(), Uuid::new_v4().simple());
        let args = build_run_args(
            self.runtime,
            &name,
            &self.image,
            &self.network,
            &self.mounts,
            &plan,
        )?;
        let mut command = TokioCommand::new(&self.cli);
        command
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let child = command.spawn().map_err(|error| {
            oci_error(
                self.runtime,
                "SANDBOX_OCI_RUN_FAILED",
                format!("Failed to start {} run: {error}", self.runtime.cli_name()),
                "launch",
                "Verify the container engine is running and the configured image can start.",
            )
        })?;

        let cleanup = OciContainerGuard {
            cli: self.cli.clone(),
            name: name.clone(),
        };
        let kill_hook = container_kill_hook(self.cli.clone(), name);
        Ok(ProcessChild::from_tokio(child)
            .with_process_tree_contained(true)
            .with_kill_hook(kill_hook)
            .with_backend_lifetime(cleanup))
    }
}

impl OciPreparedSandbox {
    fn container_path_for_host(&self, path: &Path) -> Option<String> {
        container_path_for_host(&self.mounts, path)
    }
}

struct OciContainerGuard {
    cli: PathBuf,
    name: String,
}

impl Drop for OciContainerGuard {
    fn drop(&mut self) {
        let _ = Command::new(&self.cli)
            .args(["rm", "-f", &self.name])
            .output();
    }
}

fn selected_image(
    runtime: OciRuntime,
    options: &BTreeMap<String, String>,
) -> WorkspaceResult<String> {
    let image = options
        .get(runtime.image_option_id())
        .map(String::as_str)
        .unwrap_or(DEFAULT_IMAGE)
        .trim();
    let image = if image.is_empty() {
        DEFAULT_IMAGE
    } else {
        image
    };
    if image.chars().any(char::is_whitespace) || image.chars().any(char::is_control) {
        return Err(oci_error(
            runtime,
            "SANDBOX_OCI_IMAGE_INVALID",
            format!(
                "{} container image contains whitespace or control characters.",
                runtime_label(runtime)
            ),
            "image",
            "Use an OCI image reference such as ubuntu:24.04 or registry.example.com/team/dev:tag.",
        ));
    }
    Ok(image.to_string())
}

fn selected_network(
    runtime: OciRuntime,
    options: &BTreeMap<String, String>,
) -> WorkspaceResult<String> {
    let network = options
        .get(runtime.network_option_id())
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
        return Err(oci_error(
            runtime,
            "SANDBOX_OCI_NETWORK_INVALID",
            format!(
                "{} network name contains unsupported characters.",
                runtime_label(runtime)
            ),
            "network",
            "Use 'none', 'bridge', or a simple engine network name containing letters, digits, '.', '_' or '-'.",
        ));
    }
    if network.eq_ignore_ascii_case("host") {
        return Err(oci_error(
            runtime,
            "SANDBOX_OCI_NETWORK_FORBIDDEN",
            format!(
                "{} host networking is not allowed because it escapes the container network namespace.",
                runtime_label(runtime)
            ),
            "network",
            "Use 'none' (default) or 'bridge'. Host networking is a sandbox escape.",
        ));
    }
    Ok(network.to_string())
}

fn ensure_engine_ready(runtime: OciRuntime, cli: &Path) -> WorkspaceResult<()> {
    let output = run_cli(runtime, cli, ["info"])?;
    if output.status.success() {
        return Ok(());
    }
    Err(oci_output_error_with_suggestion(
        runtime,
        "SANDBOX_OCI_UNAVAILABLE",
        &format!("{} engine is not ready.", runtime_label(runtime)),
        "discovery",
        &output,
        match runtime {
            OciRuntime::Docker => {
                "Start Docker Desktop or the Docker daemon, then retry. The app does not start the engine for you."
            }
            OciRuntime::Podman => {
                "Start the Podman machine (`podman machine start`) or the Podman service, then retry. The app does not start it for you."
            }
        },
    ))
}

fn ensure_image(runtime: OciRuntime, cli: &Path, image: &str) -> WorkspaceResult<()> {
    let inspected = run_cli(runtime, cli, ["image", "inspect", image])?;
    if inspected.status.success() {
        return Ok(());
    }
    let pulled = run_cli(runtime, cli, ["pull", image])?;
    if pulled.status.success() {
        return Ok(());
    }
    Err(oci_output_error_with_suggestion(
        runtime,
        "SANDBOX_OCI_IMAGE_UNAVAILABLE",
        &format!(
            "{} could not inspect or pull the configured container image.",
            runtime_label(runtime)
        ),
        "image",
        &pulled,
        "Verify the image reference and registry/network access, then retry.",
    ))
}

fn canonical_existing_directory(
    runtime: OciRuntime,
    path: &Path,
    label: &str,
) -> WorkspaceResult<PathBuf> {
    if is_unsupported_remote_path(path) {
        return Err(oci_error(
            runtime,
            "SANDBOX_OCI_PATH_UNSUPPORTED",
            format!("Network-backed host paths are not supported: {}", path.display()),
            "mounts",
            "Use a local filesystem directory or a WSL folder path (\\\\wsl.localhost\\<distro>\\...).",
        ));
    }
    let canonical = path.canonicalize().map_err(|error| {
        oci_error(
            runtime,
            "SANDBOX_OCI_PATH_INVALID",
            format!("{label} is unavailable: {}: {error}", path.display()),
            "mounts",
            "Use an existing local directory.",
        )
    })?;
    if !canonical.is_dir() {
        return Err(oci_error(
            runtime,
            "SANDBOX_OCI_PATH_INVALID",
            format!("{label} must be a directory: {}", path.display()),
            "mounts",
            "OCI sandbox path grants must reference directories.",
        ));
    }
    if is_unsupported_remote_path(&canonical) {
        return Err(oci_error(
            runtime,
            "SANDBOX_OCI_PATH_UNSUPPORTED",
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
    runtime: OciRuntime,
    workspace_root: &Path,
    grants: &[SandboxPathGrant],
) -> WorkspaceResult<Vec<OciMount>> {
    let workspace = canonical_existing_directory(runtime, workspace_root, "workspace root")?;
    let mut external = BTreeMap::<PathBuf, SandboxPathAccess>::new();
    for grant in grants {
        if grant.path.trim().is_empty() {
            continue;
        }
        let raw = PathBuf::from(grant.path.trim());
        let path = canonical_existing_directory(runtime, &raw, "external sandbox path")?;
        if path.starts_with(&workspace) {
            continue;
        }
        if workspace.starts_with(&path) {
            return Err(oci_error(
                runtime,
                "SANDBOX_OCI_MOUNT_OVERLAP",
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
                return Err(oci_error(
                    runtime,
                    "SANDBOX_OCI_MOUNT_OVERLAP",
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

    let mut mounts = vec![OciMount {
        host: workspace,
        container: "/workspace".into(),
        access: SandboxPathAccess::Modify,
    }];
    mounts.extend(
        external
            .into_iter()
            .enumerate()
            .map(|(index, (host, access))| OciMount {
                host,
                container: format!("/ctmcp/grants/{index}"),
                access,
            }),
    );
    Ok(mounts)
}

fn build_run_args(
    runtime: OciRuntime,
    name: &str,
    image: &str,
    network: &str,
    mounts: &[OciMount],
    plan: &SandboxProcessPlan,
) -> WorkspaceResult<Vec<String>> {
    let cwd = plan.process.cwd.as_ref().ok_or_else(|| {
        oci_error(
            runtime,
            "SANDBOX_PROCESS_PLAN_INVALID",
            format!(
                "{} process plan requires a working directory.",
                runtime_label(runtime)
            ),
            "launch",
            "Provide a workspace working directory.",
        )
    })?;
    let container_cwd = container_path_for_host(mounts, cwd).ok_or_else(|| {
        oci_error(
            runtime,
            "SANDBOX_OCI_PATH_UNMOUNTED",
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
            oci_error(
                runtime,
                "SANDBOX_OCI_COMMAND_UNMOUNTED",
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
        return Err(oci_error(
            runtime,
            "SANDBOX_PROCESS_PLAN_INVALID",
            format!(
                "{} process plan has an empty command.",
                runtime_label(runtime)
            ),
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
        "--security-opt".to_string(),
        "no-new-privileges".to_string(),
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

fn container_path_for_host(mounts: &[OciMount], path: &Path) -> Option<String> {
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

fn container_kill_hook(cli: PathBuf, name: String) -> ProcessKillHook {
    Arc::new(move || cancel_container(&cli, &name))
}

fn cancel_container(cli: &Path, name: &str) -> io::Result<()> {
    let mut last_error = String::new();
    for _ in 0..20 {
        match Command::new(cli).args(["rm", "-f", name]).output() {
            Ok(output) if output.status.success() || container_missing(&output) => return Ok(()),
            Ok(output) => {
                last_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            }
            Err(error) => last_error = error.to_string(),
        }
        thread::sleep(Duration::from_millis(50));
    }
    Err(io::Error::other(format!(
        "container cancellation failed for {name}: {last_error}"
    )))
}

fn container_missing(output: &Output) -> bool {
    let message = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    message.contains("no such container")
        || message.contains("no such object")
        || message.contains("not found")
}

fn run_cli<'a>(
    runtime: OciRuntime,
    cli: &Path,
    args: impl IntoIterator<Item = &'a str>,
) -> WorkspaceResult<Output> {
    Command::new(cli).args(args).output().map_err(|error| {
        oci_error(
            runtime,
            "SANDBOX_OCI_UNAVAILABLE",
            format!("Failed to execute {} CLI: {error}", runtime.cli_name()),
            "cli",
            "Install or repair the container engine, then retry.",
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

fn runtime_label(runtime: OciRuntime) -> &'static str {
    match runtime {
        OciRuntime::Docker => "Docker",
        OciRuntime::Podman => "Podman",
    }
}

fn oci_output_error_with_suggestion(
    runtime: OciRuntime,
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
            "backend": runtime.id(),
            "stage": stage,
            "fallback_allowed": false,
            "exit_code": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr),
            "suggestion": suggestion,
        }),
    }
}

fn oci_error(
    runtime: OciRuntime,
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
            "backend": runtime.id(),
            "stage": stage,
            "fallback_allowed": false,
            "suggestion": suggestion,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dirs() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let root = tempfile::tempdir().expect("root");
        let workspace = root.path().join("workspace");
        let readonly = root.path().join("readonly");
        let writable = root.path().join("writable");
        for path in [&workspace, &readonly, &writable] {
            std::fs::create_dir_all(path).expect("fixture");
        }
        (root, workspace, readonly, writable)
    }

    #[test]
    fn default_image_and_network_are_isolation_first() {
        assert_eq!(DEFAULT_IMAGE, "ubuntu:24.04");
        assert_eq!(
            selected_image(OciRuntime::Docker, &BTreeMap::new()).expect("docker image"),
            DEFAULT_IMAGE
        );
        assert_eq!(
            selected_network(OciRuntime::Podman, &BTreeMap::new()).expect("podman network"),
            "none"
        );
    }

    #[test]
    fn host_network_is_rejected_as_a_sandbox_escape() {
        let mut options = BTreeMap::new();
        options.insert("docker.network".into(), "host".into());
        let error = selected_network(OciRuntime::Docker, &options).expect_err("host network");
        assert_eq!(
            error.to_error_value()["code"],
            "SANDBOX_OCI_NETWORK_FORBIDDEN"
        );
    }

    #[test]
    fn whitespace_image_is_rejected() {
        let mut options = BTreeMap::new();
        options.insert("podman.image".into(), "ubuntu:24.04 alpine".into());
        let error = selected_image(OciRuntime::Podman, &options).expect_err("invalid image");
        assert_eq!(error.to_error_value()["code"], "SANDBOX_OCI_IMAGE_INVALID");
    }

    #[test]
    fn mount_model_assigns_private_container_paths_and_preserves_access() {
        let (_root, workspace, readonly, writable) = fixture_dirs();
        let mounts = build_mounts(
            OciRuntime::Docker,
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
    fn file_grants_fail_closed_instead_of_broadening_to_parent_directory() {
        let (_root, workspace, _, _) = fixture_dirs();
        let file = workspace.parent().unwrap().join("single.txt");
        std::fs::write(&file, "secret").expect("fixture");
        let error = build_mounts(
            OciRuntime::Podman,
            &workspace,
            &[SandboxPathGrant {
                path: file.to_string_lossy().into_owned(),
                access: SandboxPathAccess::ReadOnly,
            }],
        )
        .expect_err("file grant must fail closed");
        assert_eq!(error.to_error_value()["code"], "SANDBOX_OCI_PATH_INVALID");
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
            OciRuntime::Docker,
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
        .expect_err("nested read-only must not be weakened");
        assert_eq!(error.to_error_value()["code"], "SANDBOX_OCI_MOUNT_OVERLAP");
    }

    #[test]
    fn run_args_create_one_ephemeral_container_with_explicit_mounts() {
        let (_root, workspace, _, _) = fixture_dirs();
        let mounts = build_mounts(OciRuntime::Docker, &workspace, &[]).expect("mounts");
        let plan = SandboxProcessPlan {
            backend_id: DOCKER_BACKEND_ID.into(),
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
        let args = build_run_args(
            OciRuntime::Docker,
            "ctmcp-test",
            DEFAULT_IMAGE,
            DEFAULT_NETWORK,
            &mounts,
            &plan,
        )
        .expect("args");
        assert_eq!(&args[..5], ["run", "--rm", "-i", "--name", "ctmcp-test"]);
        assert!(args.windows(2).any(|pair| pair == ["--network", "none"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--security-opt", "no-new-privileges"]));
        assert!(args.contains(&"-v".to_string()));
        assert!(args.iter().any(|value| value.ends_with(":/workspace")));
        assert!(args.windows(2).any(|pair| pair == ["-u", "DROP"]));
        assert!(args.windows(2).any(|pair| pair == ["-e", "KEEP=1"]));
        assert_eq!(&args[args.len() - 2..], ["python", "--version"]);
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

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn live_docker_provider_when_explicitly_enabled() {
        if std::env::var("CTMCP_TEST_DOCKER").as_deref() != Ok("1") {
            return;
        }
        live_oci_provider(OciRuntime::Docker).await;
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn live_podman_provider_when_explicitly_enabled() {
        if std::env::var("CTMCP_TEST_PODMAN").as_deref() != Ok("1") {
            return;
        }
        live_oci_provider(OciRuntime::Podman).await;
    }

    async fn live_oci_provider(runtime: OciRuntime) {
        use tokio::io::AsyncWriteExt;

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

        let workspace_model = Workspace::new(workspace.clone()).expect("workspace");
        let mut options = BTreeMap::new();
        options.insert(
            runtime.image_option_id().into(),
            std::env::var(match runtime {
                OciRuntime::Docker => "CTMCP_TEST_DOCKER_IMAGE",
                OciRuntime::Podman => "CTMCP_TEST_PODMAN_IMAGE",
            })
            .unwrap_or_else(|_| "alpine:3.20".into()),
        );
        options.insert(runtime.network_option_id().into(), "none".into());
        let grants = vec![
            SandboxPathGrant {
                path: readonly.to_string_lossy().into_owned(),
                access: SandboxPathAccess::ReadOnly,
            },
            SandboxPathGrant {
                path: writable.to_string_lossy().into_owned(),
                access: SandboxPathAccess::Modify,
            },
        ];
        let provider = prepare(runtime, &workspace_model, &grants, &options).expect("prepare");

        let plan = provider
            .prepare_command(
                SandboxCommand::new(
                    PathBuf::from("sh"),
                    vec![
                        "-lc".into(),
                        "printf '%s|' \"$CTMCP_LIVE\"; cat; printf workspace-write > live.txt"
                            .into(),
                    ],
                    workspace.clone(),
                ),
                vec![("CTMCP_LIVE".into(), "ok".into())],
                Vec::new(),
            )
            .expect("live process plan");
        let mut child = provider
            .launch_prepared_process(plan)
            .expect("live process launch");
        {
            let mut writer = child.take_stdin().expect("live stdin");
            writer
                .write_all(b"stdin-ok")
                .await
                .expect("write live stdin");
            writer.shutdown().await.expect("shutdown live stdin");
        }
        let output = child.wait_with_output().await.expect("live process output");
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

        let mounts = build_mounts(runtime, &workspace, &grants).expect("live mounts");
        let readonly_vm = container_path_for_host(&mounts, &readonly.join("readonly.txt"))
            .expect("readonly container path");
        let plan = provider
            .prepare_command(
                SandboxCommand::new(
                    PathBuf::from("sh"),
                    vec!["-lc".into(), format!("cat '{readonly_vm}'")],
                    workspace.clone(),
                ),
                Vec::new(),
                Vec::new(),
            )
            .expect("readonly plan");
        let child = provider
            .launch_prepared_process(plan)
            .expect("readonly launch");
        let output = child.wait_with_output().await.expect("readonly output");
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "readonly-marker"
        );
        assert!(container_path_for_host(&mounts, &hidden.join("secret.txt")).is_none());
    }
}
