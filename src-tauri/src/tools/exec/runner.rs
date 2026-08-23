use std::path::{Path, PathBuf};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use tokio::process::Command;

use crate::platform::wsl::invocation_for_path;
use crate::tools::process_spec::ProcessLaunchSpec;

use super::spec::{detected_powershell, powershell_literal, powershell_script, ExecSpec};

#[derive(Clone, Copy)]
pub(super) enum CommandIoMode {
    Session,
    PostCheck,
}

pub(super) type PreparedProcessSpec = ProcessLaunchSpec;

pub(super) fn prepared_process_spec(spec: &ExecSpec, cwd: &Path) -> PreparedProcessSpec {
    prepare_process_launch_spec(
        Path::new(&spec.program),
        &spec.args,
        cwd,
        &spec.env,
        &spec.remove_env,
    )
}

pub(crate) fn prepare_process_launch_spec(
    program: &Path,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    remove_env: &[String],
) -> PreparedProcessSpec {
    let program_text = program.to_string_lossy();
    let wsl_invocation = invocation_for_path(cwd, &program_text, args, env, remove_env);
    let mut prepared = if let Some(invocation) = wsl_invocation {
        PreparedProcessSpec {
            program: PathBuf::from(invocation.program),
            args: invocation.args,
            cwd: None,
            env: Vec::new(),
            remove_env: Vec::new(),
            required_env: Vec::new(),
            windows_raw_arg: None,
            using_wsl: true,
        }
    } else {
        let mut prepared = process_spec_for_program(&program_text, args);
        prepared.cwd = Some(platform_command_path(cwd));
        prepared.env = env.to_vec();
        prepared.remove_env = remove_env.to_vec();
        prepared
    };

    #[cfg(windows)]
    {
        // Preserve the historical ordering: these built-ins are applied after caller
        // env/remove_env mutations, so they win even when the caller supplied or removed
        // the same key.
        prepared.required_env.extend([
            ("PYTHONUTF8".into(), "1".into()),
            ("PYTHONIOENCODING".into(), "utf-8".into()),
            ("PYTHONLEGACYWINDOWSSTDIO".into(), "0".into()),
        ]);
    }

    prepared
}

pub(super) fn prepared_command(spec: &ExecSpec, cwd: &Path, io_mode: CommandIoMode) -> Command {
    let prepared = prepared_process_spec(spec, cwd);
    command_from_process_spec(&prepared, io_mode)
}

pub(super) fn command_from_process_spec(
    prepared: &PreparedProcessSpec,
    io_mode: CommandIoMode,
) -> Command {
    let mut command = Command::new(&prepared.program);
    command.env_clear();
    inherit_safe_parent_environment(&mut command);
    command.args(&prepared.args);
    #[cfg(windows)]
    if let Some(raw_arg) = prepared.windows_raw_arg.as_deref() {
        command.as_std_mut().raw_arg(raw_arg);
    }
    for (key, value) in &prepared.env {
        command.env(key, value);
    }
    for key in &prepared.remove_env {
        command.env_remove(key);
    }
    for (key, value) in &prepared.required_env {
        command.env(key, value);
    }
    if let Some(cwd) = prepared.cwd.as_deref() {
        command.current_dir(cwd);
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
            .as_std_mut()
            .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    command
}

fn inherit_safe_parent_environment(command: &mut Command) {
    for (key, value) in std::env::vars_os() {
        if key
            .to_str()
            .is_some_and(should_inherit_parent_environment_key)
        {
            command.env(key, value);
        }
    }
}

fn should_inherit_parent_environment_key(key: &str) -> bool {
    let key = key.to_ascii_uppercase();
    if key.starts_with("LC_") {
        return true;
    }
    matches!(
        key.as_str(),
        "PATH"
            | "PATHEXT"
            | "SYSTEMROOT"
            | "WINDIR"
            | "COMSPEC"
            | "TEMP"
            | "TMP"
            | "TMPDIR"
            | "HOME"
            | "USER"
            | "LOGNAME"
            | "USERPROFILE"
            | "HOMEDRIVE"
            | "HOMEPATH"
            | "LOCALAPPDATA"
            | "APPDATA"
            | "PROGRAMDATA"
            | "PROGRAMFILES"
            | "PROGRAMFILES(X86)"
            | "PROGRAMW6432"
            | "LANG"
            | "LANGUAGE"
            | "SHELL"
            | "TERM"
            | "COLORTERM"
            | "CARGO_HOME"
            | "RUSTUP_HOME"
            | "GOPATH"
            | "GOROOT"
            | "JAVA_HOME"
            | "GRADLE_HOME"
            | "MAVEN_HOME"
            | "NPM_CONFIG_PREFIX"
            | "PNPM_HOME"
            | "COREPACK_HOME"
            | "VIRTUAL_ENV"
            | "CONDA_PREFIX"
            | "PYENV_ROOT"
            | "NVM_HOME"
            | "NVM_SYMLINK"
            | "VOLTA_HOME"
            | "XDG_CONFIG_HOME"
            | "XDG_CACHE_HOME"
            | "XDG_DATA_HOME"
            | "XDG_STATE_HOME"
            | "SSL_CERT_FILE"
            | "SSL_CERT_DIR"
            | "NODE_EXTRA_CA_CERTS"
            | "REQUESTS_CA_BUNDLE"
            | "CURL_CA_BUNDLE"
    )
}

pub(super) fn process_spec_for_program(program: &str, args: &[String]) -> PreparedProcessSpec {
    #[cfg(windows)]
    {
        let extension = Path::new(program)
            .extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase);
        match extension.as_deref() {
            Some("bat") | Some("cmd") => {
                return PreparedProcessSpec {
                    program: PathBuf::from("cmd.exe"),
                    args: vec!["/d".into(), "/s".into(), "/c".into()],
                    cwd: None,
                    env: Vec::new(),
                    remove_env: Vec::new(),
                    required_env: Vec::new(),
                    windows_raw_arg: Some(windows_batch_command_line(program, args)),
                    using_wsl: false,
                };
            }
            Some("ps1") => {
                let shell = detected_powershell()
                    .map(|runtime| PathBuf::from(&runtime.program))
                    .unwrap_or_else(|| PathBuf::from("powershell.exe"));
                let mut invocation = format!(
                    "& {}",
                    powershell_literal(windows_command_path(program).as_str())
                );
                for argument in args {
                    invocation.push(' ');
                    invocation.push_str(&powershell_literal(argument));
                }
                let script = powershell_script(&invocation);
                return PreparedProcessSpec {
                    program: shell,
                    args: vec![
                        "-NoLogo".into(),
                        "-NoProfile".into(),
                        "-NonInteractive".into(),
                        "-ExecutionPolicy".into(),
                        "Bypass".into(),
                        "-Command".into(),
                        script,
                    ],
                    cwd: None,
                    env: Vec::new(),
                    remove_env: Vec::new(),
                    required_env: Vec::new(),
                    windows_raw_arg: None,
                    using_wsl: false,
                };
            }
            _ => {}
        }
    }

    PreparedProcessSpec {
        program: PathBuf::from(program),
        args: args.to_vec(),
        cwd: None,
        env: Vec::new(),
        remove_env: Vec::new(),
        required_env: Vec::new(),
        windows_raw_arg: None,
        using_wsl: false,
    }
}

#[cfg(test)]
pub(super) fn command_for_program(program: &str, args: &[String]) -> Command {
    command_from_process_spec(
        &process_spec_for_program(program, args),
        CommandIoMode::Session,
    )
}

#[cfg(windows)]
pub(super) fn windows_batch_command_line(program: &str, args: &[String]) -> String {
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

fn platform_command_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(windows_command_path(&path.to_string_lossy()))
    }
    #[cfg(not(windows))]
    path.to_path_buf()
}

#[cfg(windows)]
fn windows_command_path(path: &str) -> String {
    path.strip_prefix("\\\\?\\").unwrap_or(path).to_string()
}

#[cfg(test)]
mod environment_tests {
    use super::should_inherit_parent_environment_key;

    #[test]
    fn child_environment_keeps_runtime_paths_but_not_common_credentials() {
        for key in ["PATH", "SystemRoot", "HOME", "LC_MESSAGES", "CARGO_HOME"] {
            assert!(should_inherit_parent_environment_key(key), "{key}");
        }
        for key in [
            "OPENAI_API_KEY",
            "AWS_SECRET_ACCESS_KEY",
            "GITHUB_TOKEN",
            "SSH_AUTH_SOCK",
            "HTTP_PROXY",
        ] {
            assert!(!should_inherit_parent_environment_key(key), "{key}");
        }
    }
}
