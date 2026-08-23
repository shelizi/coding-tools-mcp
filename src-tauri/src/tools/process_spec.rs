use std::path::PathBuf;

/// Concrete host-process launch shape after shell/script/WSL normalization.
///
/// This type is intentionally sandbox-neutral. Native Tokio execution and sandbox
/// providers consume the same representation so platform quoting, cwd and environment
/// semantics cannot fork between backends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProcessLaunchSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub remove_env: Vec<String>,
    pub required_env: Vec<(String, String)>,
    pub windows_raw_arg: Option<String>,
    pub using_wsl: bool,
}
