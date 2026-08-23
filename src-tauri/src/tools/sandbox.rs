use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::json;

use crate::tools::process_child::ProcessChild;
use crate::tools::process_spec::ProcessLaunchSpec;
use crate::workspace::SandboxConfig;

use super::workspace::{Workspace, WorkspaceError, WorkspaceResult};

#[cfg(windows)]
mod appcontainer;
#[cfg(windows)]
pub(crate) use appcontainer::run_acl_helper_if_requested as run_appcontainer_acl_helper_if_requested;
mod docker_sbx;
mod oci;
mod wslc;

pub const APPCONTAINER_NETWORK_OPTION_ID: &str = "appcontainer.network";
pub const APPCONTAINER_DEFAULT_NETWORK: &str = "none";

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SandboxBackendOptionDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub placeholder: String,
    pub default_value: String,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SandboxBackendDescriptor {
    pub id: String,
    pub label: String,
    pub description: String,
    pub host_supported: bool,
    pub supports_wsl: bool,
    pub enforcement_ready: bool,
    pub experimental: bool,
    pub options: Vec<SandboxBackendOptionDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxCommand {
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

impl SandboxCommand {
    pub fn new(executable: PathBuf, args: Vec<String>, cwd: PathBuf) -> Self {
        Self {
            executable,
            args,
            cwd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxStatePersistence {
    Session,
    Workspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStateLayout {
    pub root: PathBuf,
    pub home: PathBuf,
    pub temp: PathBuf,
    pub cache: PathBuf,
    pub persistence: SandboxStatePersistence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SandboxProcessPlan {
    pub backend_id: String,
    pub process: ProcessLaunchSpec,
    pub environment_overrides: BTreeMap<String, String>,
    pub state: Option<SandboxStateLayout>,
}

/// Provider-owned prepared sandbox lifetime.
///
/// A concrete implementation may hold OS resources such as an AppContainer profile/SID,
/// capability grants, namespace/mount handles or a broker session. Releasing the boxed
/// value is the cleanup boundary, so backend-specific lifetime state does not leak into
/// generic `exec_command` orchestration.
pub(crate) trait PreparedSandbox: Send + Sync {
    fn backend_id(&self) -> &str;

    fn state_layout(&self) -> Option<&SandboxStateLayout> {
        None
    }

    /// First-stage provider hook for logical tools before generic platform normalization.
    /// For example AppContainer rewrites Scoop `npm.cmd` to physical Node + npm-cli.js here,
    /// before generic Windows `.cmd` handling would otherwise hide npm behind `cmd.exe`.
    fn normalize_logical_command(
        &self,
        command: SandboxCommand,
    ) -> WorkspaceResult<SandboxCommand> {
        Ok(command)
    }

    /// Build the provider-specific process plan from a logical command. Host-native
    /// providers use the generic platform normalizer; cross-OS providers can override
    /// this stage so a Linux sandbox never receives a Windows-resolved executable.
    fn prepare_command(
        &self,
        command: SandboxCommand,
        env: Vec<(String, String)>,
        remove_env: Vec<String>,
    ) -> WorkspaceResult<SandboxProcessPlan> {
        let logical = self.normalize_logical_command(command)?;
        let process = crate::tools::exec::prepare_process_launch_spec(
            &logical.executable,
            &logical.args,
            &logical.cwd,
            &env,
            &remove_env,
        );
        self.prepare_process(process)
    }

    /// Second-stage provider hook after generic shell/script/WSL normalization has produced
    /// the exact host process shape. Runtime grants belong here because the final launcher
    /// may differ from the logical tool (for example a `.ps1` becomes PowerShell).
    fn prepare_process(&self, process: ProcessLaunchSpec) -> WorkspaceResult<SandboxProcessPlan> {
        let environment_overrides = self.environment_overrides()?;
        Ok(SandboxProcessPlan {
            backend_id: self.backend_id().to_string(),
            process,
            environment_overrides,
            state: self.state_layout().cloned(),
        })
    }

    /// Launch a process that has already passed this provider's concrete preparation.
    /// The returned ProcessChild must own every provider resource needed after launch.
    fn launch_prepared_process(&self, _plan: SandboxProcessPlan) -> WorkspaceResult<ProcessChild> {
        Err(sandbox_error(
            "SANDBOX_BACKEND_NOT_READY",
            format!(
                "Sandbox backend '{}' does not implement prepared-process launch.",
                self.backend_id()
            ),
            self.backend_id(),
            "research",
        ))
    }

    /// Return only backend-owned environment overrides. Generic execution remains
    /// responsible for combining these with caller/environment policy in one place.
    fn environment_overrides(&self) -> WorkspaceResult<BTreeMap<String, String>> {
        Ok(BTreeMap::new())
    }
}

/// Backend-neutral provider contract used by settings, UI discovery and execution.
///
/// `prepare` is intentionally provider-owned: it is where a future AppContainer backend
/// can acquire a per-workspace package SID/profile, provision the private runtime
/// capability and create managed state, while a WSL/container backend can prepare its
/// own mounts/namespaces. The returned `PreparedSandbox` then owns command normalization,
/// environment overrides, process launch and cleanup lifetime. A provider may only claim
/// `enforcement_ready` once that launch bridge and its validation gates are wired end-to-end.
pub(crate) trait SandboxBackend: Send + Sync {
    fn descriptor(&self) -> SandboxBackendDescriptor;

    fn supports_workspace(&self, workspace: &Workspace) -> bool {
        let descriptor = self.descriptor();
        descriptor.host_supported && (descriptor.supports_wsl || !workspace.is_wsl())
    }

    fn prepare(
        &self,
        _workspace: &Workspace,
        _config: &SandboxConfig,
    ) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
        let descriptor = self.descriptor();
        Err(sandbox_error(
            "SANDBOX_BACKEND_NOT_READY",
            format!(
                "Sandbox backend '{}' does not have a production preparation/launch path yet.",
                descriptor.label
            ),
            &descriptor.id,
            "research",
        ))
    }
}

struct AppContainerBackend;

impl SandboxBackend for AppContainerBackend {
    fn descriptor(&self) -> SandboxBackendDescriptor {
        SandboxBackendDescriptor {
            id: "appcontainer".into(),
            label: "Windows AppContainer".into(),
            description:
                "OS-enforced per-workspace isolation with a shared private runtime capability. Windows host folders only; WSL folders need Docker, Podman, Docker Sandboxes, or WSL Containers."
                    .into(),
            host_supported: cfg!(windows),
            supports_wsl: false,
            // R6 production validation covers package identity, workspace/outside mutation,
            // descendants, shared session lifecycle, representative runtimes, multi-workspace
            // isolation and structured fail-closed startup errors.
            enforcement_ready: true,
            experimental: true,
            options: vec![SandboxBackendOptionDescriptor {
                id: APPCONTAINER_NETWORK_OPTION_ID.into(),
                label: "Network access".into(),
                description: "AppContainer network capability. 'none' is the isolation-first default; 'internet' grants the well-known internetClient capability for package installs and clones.".into(),
                placeholder: APPCONTAINER_DEFAULT_NETWORK.into(),
                default_value: APPCONTAINER_DEFAULT_NETWORK.into(),
                required: true,
            }],
        }
    }

    fn prepare(
        &self,
        workspace: &Workspace,
        config: &SandboxConfig,
    ) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
        #[cfg(windows)]
        {
            appcontainer::prepare(workspace, &config.external_paths, &config.options)
        }
        #[cfg(not(windows))]
        {
            let _ = workspace;
            Err(sandbox_error(
                "SANDBOX_BACKEND_UNSUPPORTED",
                "Windows AppContainer is not available on this host.".into(),
                "appcontainer",
                "unsupported",
            ))
        }
    }
}

struct DockerBackend;

impl SandboxBackend for DockerBackend {
    fn descriptor(&self) -> SandboxBackendDescriptor {
        SandboxBackendDescriptor {
            id: oci::DOCKER_BACKEND_ID.into(),
            label: "Docker".into(),
            description: "Native Docker Linux containers. Each command runs in an ephemeral container with the workspace and explicit grants bind-mounted. Network is denied by default. Supports Windows host folders, WSL folders, Linux, and macOS when the Docker engine is running.".into(),
            host_supported: oci::discover_docker_program().is_some(),
            supports_wsl: true,
            enforcement_ready: true,
            experimental: true,
            options: vec![
                SandboxBackendOptionDescriptor {
                    id: "docker.image".into(),
                    label: "Container image".into(),
                    description: "OCI image used for each isolated command container. The image must contain the tools you want to execute.".into(),
                    placeholder: oci::DEFAULT_IMAGE.into(),
                    default_value: oci::DEFAULT_IMAGE.into(),
                    required: true,
                },
                SandboxBackendOptionDescriptor {
                    id: "docker.network".into(),
                    label: "Container network".into(),
                    description: "Docker network attached to command containers. 'none' is the isolation-first default; use 'bridge' or a named network when commands need networking. Host networking is rejected.".into(),
                    placeholder: oci::DEFAULT_NETWORK.into(),
                    default_value: oci::DEFAULT_NETWORK.into(),
                    required: true,
                },
            ],
        }
    }

    fn prepare(
        &self,
        workspace: &Workspace,
        config: &SandboxConfig,
    ) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
        oci::prepare(
            oci::OciRuntime::Docker,
            workspace,
            &config.external_paths,
            &config.options,
        )
    }
}

struct PodmanBackend;

impl SandboxBackend for PodmanBackend {
    fn descriptor(&self) -> SandboxBackendDescriptor {
        SandboxBackendDescriptor {
            id: oci::PODMAN_BACKEND_ID.into(),
            label: "Podman".into(),
            description: "Native Podman Linux containers. Each command runs in an ephemeral container with the workspace and explicit grants bind-mounted. Network is denied by default. Supports Windows host folders, WSL folders, Linux, and macOS when the Podman engine is running.".into(),
            host_supported: oci::discover_podman_program().is_some(),
            supports_wsl: true,
            enforcement_ready: true,
            experimental: true,
            options: vec![
                SandboxBackendOptionDescriptor {
                    id: "podman.image".into(),
                    label: "Container image".into(),
                    description: "OCI image used for each isolated command container. The image must contain the tools you want to execute.".into(),
                    placeholder: oci::DEFAULT_IMAGE.into(),
                    default_value: oci::DEFAULT_IMAGE.into(),
                    required: true,
                },
                SandboxBackendOptionDescriptor {
                    id: "podman.network".into(),
                    label: "Container network".into(),
                    description: "Podman network attached to command containers. 'none' is the isolation-first default; use 'bridge' or a named network when commands need networking. Host networking is rejected.".into(),
                    placeholder: oci::DEFAULT_NETWORK.into(),
                    default_value: oci::DEFAULT_NETWORK.into(),
                    required: true,
                },
            ],
        }
    }

    fn prepare(
        &self,
        workspace: &Workspace,
        config: &SandboxConfig,
    ) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
        oci::prepare(
            oci::OciRuntime::Podman,
            workspace,
            &config.external_paths,
            &config.options,
        )
    }
}

struct DockerSbxBackend;

impl SandboxBackend for DockerSbxBackend {
    fn descriptor(&self) -> SandboxBackendDescriptor {
        SandboxBackendDescriptor {
            id: "docker_sbx".into(),
            label: "Docker Sandboxes (sbx)".into(),
            description: "Linux microVM sandbox using Docker sbx. The primary workspace is a direct read-write mount; explicit external directories honor read-only/modify grants. Supports Windows host folders and WSL folders via WSL UNC mounts.".into(),
            host_supported: docker_sbx::discover_sbx_program().is_some(),
            supports_wsl: true,
            enforcement_ready: true,
            experimental: true,
            options: Vec::new(),
        }
    }

    fn prepare(
        &self,
        workspace: &Workspace,
        config: &SandboxConfig,
    ) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
        docker_sbx::prepare(workspace, &config.external_paths)
    }
}

struct WslcBackend;

impl SandboxBackend for WslcBackend {
    fn descriptor(&self) -> SandboxBackendDescriptor {
        SandboxBackendDescriptor {
            id: "wslc".into(),
            label: "Microsoft WSL Containers (wslc)".into(),
            description: "Microsoft first-party Linux container sandbox managed by wslc. Host paths are exposed only through explicit bind mounts. Supports Windows host folders and WSL folders via WSL UNC mounts.".into(),
            host_supported: wslc::discover_wslc_program().is_some(),
            supports_wsl: true,
            enforcement_ready: true,
            experimental: true,
            options: vec![
                SandboxBackendOptionDescriptor {
                    id: "wslc.image".into(),
                    label: "Container image".into(),
                    description: "OCI image used for each isolated command container. The image must contain the tools you want to execute.".into(),
                    placeholder: wslc::DEFAULT_IMAGE.into(),
                    default_value: wslc::DEFAULT_IMAGE.into(),
                    required: true,
                },
                SandboxBackendOptionDescriptor {
                    id: "wslc.network".into(),
                    label: "Container network".into(),
                    description: "WSLC network attached to command containers. 'none' is the isolation-first default; use 'bridge' or a named network when commands need networking.".into(),
                    placeholder: wslc::DEFAULT_NETWORK.into(),
                    default_value: wslc::DEFAULT_NETWORK.into(),
                    required: true,
                },
            ],
        }
    }

    fn prepare(
        &self,
        workspace: &Workspace,
        config: &SandboxConfig,
    ) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
        wslc::prepare(workspace, &config.external_paths, &config.options)
    }
}

static APP_CONTAINER_BACKEND: AppContainerBackend = AppContainerBackend;
static DOCKER_BACKEND: DockerBackend = DockerBackend;
static PODMAN_BACKEND: PodmanBackend = PodmanBackend;
static DOCKER_SBX_BACKEND: DockerSbxBackend = DockerSbxBackend;
static WSLC_BACKEND: WslcBackend = WslcBackend;
static BACKENDS: [&'static dyn SandboxBackend; 5] = [
    &APP_CONTAINER_BACKEND,
    &DOCKER_BACKEND,
    &PODMAN_BACKEND,
    &DOCKER_SBX_BACKEND,
    &WSLC_BACKEND,
];

pub fn backend_descriptors() -> Vec<SandboxBackendDescriptor> {
    BACKENDS
        .iter()
        .map(|backend| backend.descriptor())
        .collect()
}

pub fn discovered_sbx_program() -> Option<PathBuf> {
    docker_sbx::discover_sbx_program()
}

pub fn discovered_wslc_program() -> Option<PathBuf> {
    wslc::discover_wslc_program()
}

pub fn discovered_docker_program() -> Option<PathBuf> {
    oci::discover_docker_program()
}

pub fn discovered_podman_program() -> Option<PathBuf> {
    oci::discover_podman_program()
}

pub fn uses_portable_command(backend_id: &str) -> bool {
    matches!(
        backend_id.trim(),
        "docker" | "podman" | "docker_sbx" | "wslc"
    )
}

pub(crate) fn backend(id: &str) -> Option<&'static dyn SandboxBackend> {
    let id = id.trim();
    BACKENDS
        .iter()
        .copied()
        .find(|backend| backend.descriptor().id == id)
}

#[cfg(test)]
pub(crate) fn prepare_backend_for_test(
    id: &str,
    workspace: &Workspace,
) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
    let backend = backend(id).ok_or_else(|| {
        sandbox_error(
            "SANDBOX_BACKEND_UNKNOWN",
            format!("Sandbox backend is not registered: {id}"),
            id,
            "unknown",
        )
    })?;
    backend.prepare(workspace, &SandboxConfig::default())
}

#[cfg(test)]
pub(crate) fn build_prepared_process_plan(
    prepared: &dyn PreparedSandbox,
    process: ProcessLaunchSpec,
) -> WorkspaceResult<SandboxProcessPlan> {
    prepared.prepare_process(process)
}

pub(crate) async fn start_prepared_sandbox_command(
    prepared: &dyn PreparedSandbox,
    command: SandboxCommand,
    env: Vec<(String, String)>,
    remove_env: Vec<String>,
) -> Result<
    crate::tools::process_start::StartedChild,
    crate::tools::process_start::ControlledProcessStartError<WorkspaceError>,
> {
    use crate::tools::process_start::{start_process_with_control, ControlledProcessStartError};

    let plan = prepared
        .prepare_command(command, env, remove_env)
        .map_err(ControlledProcessStartError::Start)?;
    start_process_with_control(|| prepared.launch_prepared_process(plan.clone())).await
}

/// Resolve and prepare an enabled provider. Future generic exec integration should call
/// this rather than bypassing the registry, so unknown/unsupported/research-only backends
/// retain the same fail-closed behavior as UI preflight.
pub(crate) fn prepare_enabled_backend(
    config: &SandboxConfig,
    workspace: &Workspace,
) -> WorkspaceResult<Box<dyn PreparedSandbox>> {
    if !config.enabled {
        return Err(sandbox_error(
            "SANDBOX_DISABLED",
            "Sandbox preparation was requested while sandboxing is disabled.".into(),
            config.backend.trim(),
            "disabled",
        ));
    }
    ensure_execution_available(config, workspace)?;
    let backend = backend(config.backend.trim()).ok_or_else(|| {
        sandbox_error(
            "SANDBOX_BACKEND_UNKNOWN",
            format!(
                "Sandbox backend is not registered: {}",
                config.backend.trim()
            ),
            config.backend.trim(),
            "unknown",
        )
    })?;
    backend.prepare(workspace, config)
}

/// Enforces the important configuration invariant: enabled sandboxing never silently
/// degrades to the policy-only execution path.
pub fn ensure_execution_available(
    config: &SandboxConfig,
    workspace: &Workspace,
) -> WorkspaceResult<()> {
    if !config.enabled {
        return Ok(());
    }

    let backend_id = config.backend.trim();
    let Some(backend) = backend(backend_id) else {
        return Err(sandbox_error(
            "SANDBOX_BACKEND_UNKNOWN",
            format!("Sandbox backend is not registered: {backend_id}"),
            backend_id,
            "unknown",
        ));
    };
    let descriptor = backend.descriptor();
    if !backend.supports_workspace(workspace) {
        return Err(sandbox_error(
            "SANDBOX_BACKEND_UNSUPPORTED",
            format!(
                "Sandbox backend '{}' is not supported for this workspace execution target.",
                descriptor.label
            ),
            &descriptor.id,
            "unsupported",
        ));
    }
    if !descriptor.enforcement_ready {
        return Err(sandbox_error(
            "SANDBOX_BACKEND_NOT_READY",
            format!(
                "Sandbox backend '{}' is enabled but its production executor is not ready. Execution is blocked instead of falling back to policy-only mode.",
                descriptor.label
            ),
            &descriptor.id,
            "research",
        ));
    }

    Ok(())
}

fn sandbox_error(
    code: &'static str,
    message: String,
    backend_id: &str,
    status: &str,
) -> WorkspaceError {
    WorkspaceError::ToolDetails {
        code,
        message,
        category: "security",
        retryable: false,
        details: json!({
            "sandbox_enabled": true,
            "sandbox_backend": backend_id,
            "sandbox_status": status,
            "fallback_allowed": false,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakePreparedSandbox {
        state: SandboxStateLayout,
    }

    impl PreparedSandbox for FakePreparedSandbox {
        fn backend_id(&self) -> &str {
            "fake"
        }

        fn state_layout(&self) -> Option<&SandboxStateLayout> {
            Some(&self.state)
        }

        fn normalize_logical_command(
            &self,
            mut command: SandboxCommand,
        ) -> WorkspaceResult<SandboxCommand> {
            if command
                .executable
                .file_name()
                .and_then(|name| name.to_str())
                == Some("npm.cmd")
            {
                command.executable = PathBuf::from("node.exe");
                command.args.insert(0, "npm-cli.js".into());
            }
            Ok(command)
        }

        fn environment_overrides(&self) -> WorkspaceResult<BTreeMap<String, String>> {
            Ok(BTreeMap::from([
                (
                    "HOME".into(),
                    self.state.home.to_string_lossy().into_owned(),
                ),
                (
                    "TEMP".into(),
                    self.state.temp.to_string_lossy().into_owned(),
                ),
            ]))
        }
    }

    #[test]
    fn registry_exposes_ready_appcontainer_backend() {
        let backends = backend_descriptors();
        let appcontainer = backends
            .iter()
            .find(|backend| backend.id == "appcontainer")
            .expect("appcontainer backend");
        assert!(appcontainer.experimental);
        assert!(appcontainer.enforcement_ready);
        assert!(!appcontainer.supports_wsl);
        assert_eq!(appcontainer.options.len(), 1);
        assert_eq!(appcontainer.options[0].id, APPCONTAINER_NETWORK_OPTION_ID);
        assert_eq!(
            appcontainer.options[0].default_value,
            APPCONTAINER_DEFAULT_NETWORK
        );
    }

    #[test]
    fn registry_exposes_native_docker_and_podman_backends() {
        let backends = backend_descriptors();
        assert_eq!(
            backends
                .iter()
                .map(|backend| backend.id.as_str())
                .collect::<Vec<_>>(),
            ["appcontainer", "docker", "podman", "docker_sbx", "wslc"]
        );
        let docker = backends
            .iter()
            .find(|backend| backend.id == "docker")
            .expect("docker backend");
        assert_eq!(docker.label, "Docker");
        assert!(docker.experimental);
        assert!(docker.enforcement_ready);
        assert!(docker.supports_wsl);
        assert_eq!(
            docker.host_supported,
            oci::discover_docker_program().is_some()
        );
        assert_eq!(docker.options[0].id, "docker.image");
        assert_eq!(docker.options[0].default_value, oci::DEFAULT_IMAGE);
        assert_eq!(docker.options[1].id, "docker.network");
        assert_eq!(docker.options[1].default_value, "none");
        let podman = backends
            .iter()
            .find(|backend| backend.id == "podman")
            .expect("podman backend");
        assert_eq!(podman.label, "Podman");
        assert!(podman.supports_wsl);
        assert_eq!(
            podman.host_supported,
            oci::discover_podman_program().is_some()
        );
        assert!(uses_portable_command("docker"));
        assert!(uses_portable_command("podman"));
        assert!(uses_portable_command("docker_sbx"));
        assert!(uses_portable_command("wslc"));
        assert!(!uses_portable_command("appcontainer"));
    }

    #[test]
    fn registry_exposes_docker_sbx_as_an_alternative_backend() {
        let backends = backend_descriptors();
        let docker = backends
            .iter()
            .find(|backend| backend.id == "docker_sbx")
            .expect("docker_sbx backend");
        assert_eq!(docker.label, "Docker Sandboxes (sbx)");
        assert!(docker.experimental);
        assert!(docker.enforcement_ready);
        assert!(docker.supports_wsl);
        assert_eq!(
            docker.host_supported,
            docker_sbx::discover_sbx_program().is_some()
        );
        assert_eq!(SandboxConfig::default().backend, "appcontainer");
    }

    #[test]
    fn registry_exposes_ready_wslc_backend() {
        let backends = backend_descriptors();
        let wslc = backends
            .iter()
            .find(|backend| backend.id == "wslc")
            .expect("wslc backend");
        assert_eq!(wslc.label, "Microsoft WSL Containers (wslc)");
        assert!(wslc.experimental);
        assert!(wslc.enforcement_ready);
        assert!(wslc.supports_wsl);
        assert_eq!(
            wslc.host_supported,
            super::wslc::discover_wslc_program().is_some()
        );
        assert_eq!(wslc.options.len(), 2);
        assert_eq!(wslc.options[0].id, "wslc.image");
        assert_eq!(wslc.options[0].default_value, super::wslc::DEFAULT_IMAGE);
        assert_eq!(wslc.options[1].id, "wslc.network");
        assert_eq!(wslc.options[1].default_value, "none");
    }

    #[test]
    fn prepared_provider_separates_logical_normalization_from_process_preparation() {
        let root = tempfile::tempdir().expect("workspace");
        let state = SandboxStateLayout {
            root: root.path().join("managed-state"),
            home: root.path().join("managed-state/home"),
            temp: root.path().join("managed-state/tmp"),
            cache: root.path().join("managed-state/cache"),
            persistence: SandboxStatePersistence::Workspace,
        };
        let prepared = FakePreparedSandbox {
            state: state.clone(),
        };
        let logical = prepared
            .normalize_logical_command(SandboxCommand::new(
                PathBuf::from("npm.cmd"),
                vec!["--version".into()],
                root.path().to_path_buf(),
            ))
            .expect("logical normalization");
        assert_eq!(logical.executable, PathBuf::from("node.exe"));
        assert_eq!(logical.args, vec!["npm-cli.js", "--version"]);

        let process = ProcessLaunchSpec {
            program: logical.executable,
            args: logical.args,
            cwd: Some(logical.cwd),
            env: Vec::new(),
            remove_env: Vec::new(),
            required_env: Vec::new(),
            windows_raw_arg: None,
            using_wsl: false,
        };
        let plan = build_prepared_process_plan(&prepared, process.clone()).expect("process plan");

        assert_eq!(plan.backend_id, "fake");
        assert_eq!(plan.process, process);
        assert_eq!(plan.state, Some(state.clone()));
        assert_eq!(
            plan.environment_overrides.get("HOME"),
            Some(&state.home.to_string_lossy().into_owned())
        );
        assert_eq!(
            plan.environment_overrides.get("TEMP"),
            Some(&state.temp.to_string_lossy().into_owned())
        );
    }

    #[test]
    fn runtime_config_without_sandbox_migrates_to_disabled_default() {
        let mut value = serde_json::to_value(crate::workspace::RuntimeConfig::default())
            .expect("serialize runtime");
        value
            .as_object_mut()
            .expect("runtime object")
            .remove("sandbox");
        let runtime: crate::workspace::RuntimeConfig =
            serde_json::from_value(value).expect("deserialize legacy runtime");
        assert_eq!(runtime.sandbox, SandboxConfig::default());
    }

    #[test]
    fn disabled_sandbox_does_not_change_current_execution_boundary() {
        let root = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        ensure_execution_available(&SandboxConfig::default(), &workspace)
            .expect("disabled sandbox");
    }

    #[test]
    fn provider_preparation_cannot_activate_a_disabled_sandbox() {
        let root = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let error = match prepare_enabled_backend(&SandboxConfig::default(), &workspace) {
            Ok(_) => panic!("disabled sandbox must not prepare a provider"),
            Err(error) => error,
        };
        assert_eq!(
            error
                .to_error_value()
                .get("code")
                .and_then(|value| value.as_str()),
            Some("SANDBOX_DISABLED")
        );
    }

    #[cfg(windows)]
    #[test]
    fn enabled_appcontainer_is_execution_available_when_ready() {
        let root = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let config = SandboxConfig {
            enabled: true,
            backend: "appcontainer".into(),
            ..SandboxConfig::default()
        };
        ensure_execution_available(&config, &workspace).expect("ready AppContainer execution");
    }

    #[cfg(windows)]
    #[test]
    fn public_provider_preparation_succeeds_when_ready() {
        let root = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let config = SandboxConfig {
            enabled: true,
            backend: "appcontainer".into(),
            ..SandboxConfig::default()
        };
        let prepared = prepare_enabled_backend(&config, &workspace)
            .expect("ready AppContainer provider preparation");
        assert_eq!(prepared.backend_id(), "appcontainer");
    }

    #[test]
    fn unknown_enabled_backend_fails_closed() {
        let root = tempfile::tempdir().expect("workspace");
        let workspace = Workspace::new(root.path().to_path_buf()).expect("workspace");
        let config = SandboxConfig {
            enabled: true,
            backend: "missing-backend".into(),
            ..SandboxConfig::default()
        };
        let error = ensure_execution_available(&config, &workspace).expect_err("must fail closed");
        assert_eq!(
            error
                .to_error_value()
                .get("code")
                .and_then(|value| value.as_str()),
            Some("SANDBOX_BACKEND_UNKNOWN")
        );
    }
}
