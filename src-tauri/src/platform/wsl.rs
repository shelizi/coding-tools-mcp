use std::path::Path;

use crate::workspace::parse_wsl_path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WslInvocation {
    pub program: String,
    pub args: Vec<String>,
}

pub(crate) fn invocation_for_path(
    cwd: &Path,
    program: &str,
    args: &[String],
    env: &[(String, String)],
    remove_env: &[String],
) -> Option<WslInvocation> {
    if !cfg!(windows) {
        return None;
    }
    let location = parse_wsl_path(cwd)?;
    let inner_program = parse_wsl_path(Path::new(program))
        .filter(|program_location| {
            program_location
                .distro
                .eq_ignore_ascii_case(&location.distro)
        })
        .map(|program_location| program_location.linux_path)
        .unwrap_or_else(|| program.replace('\\', "/"));
    let inner_args = args
        .iter()
        .map(|argument| {
            parse_wsl_path(Path::new(argument))
                .filter(|argument_location| {
                    argument_location
                        .distro
                        .eq_ignore_ascii_case(&location.distro)
                })
                .map(|argument_location| argument_location.linux_path)
                .unwrap_or_else(|| argument.clone())
        })
        .collect::<Vec<_>>();

    let mut wrapped = vec![
        "--distribution".into(),
        location.distro,
        "--cd".into(),
        location.linux_path,
        "--exec".into(),
    ];
    if !env.is_empty() || !remove_env.is_empty() {
        wrapped.push("env".into());
        for key in remove_env {
            wrapped.push("-u".into());
            wrapped.push(key.clone());
        }
        for (key, value) in env {
            wrapped.push(format!("{key}={value}"));
        }
    }
    wrapped.push(inner_program);
    wrapped.extend(inner_args);
    Some(WslInvocation {
        program: "wsl.exe".into(),
        args: wrapped,
    })
}

pub(crate) fn std_command_for_workspace_with_env(
    program: &str,
    args: &[String],
    cwd: &Path,
    env: &[(String, String)],
    remove_env: &[String],
) -> std::process::Command {
    if let Some(invocation) = invocation_for_path(cwd, program, args, env, remove_env) {
        let mut command = std::process::Command::new(invocation.program);
        command.args(invocation.args);
        command
    } else {
        let mut command = std::process::Command::new(program);
        command.args(args).current_dir(cwd);
        for (key, value) in env {
            command.env(key, value);
        }
        for key in remove_env {
            command.env_remove(key);
        }
        command
    }
}

pub(crate) fn std_command_for_workspace_clean_env(
    program: &str,
    args: &[String],
    cwd: &Path,
) -> std::process::Command {
    if let Some(invocation) = clean_env_invocation_for_path(cwd, program, args) {
        let mut command = std::process::Command::new(invocation.program);
        command.args(invocation.args);
        command
    } else {
        let mut command = std::process::Command::new(program);
        command.args(args).current_dir(cwd).env_clear();
        command
    }
}

fn clean_env_invocation_for_path(
    cwd: &Path,
    program: &str,
    args: &[String],
) -> Option<WslInvocation> {
    let mut invocation = invocation_for_path(cwd, program, args, &[], &[])?;
    if invocation.args.len() < 6 {
        return None;
    }
    let inner = invocation.args.split_off(5);
    let mut inner_args = inner.into_iter();
    let inner_program = inner_args.next()?;
    let mut prefix = invocation.args;

    const CLEAN_ENV_SCRIPT: &str = concat!(
        "exec env -i ",
        "PATH=\"$PATH\" HOME=\"$HOME\" PWD=\"$PWD\" ",
        "USER=\"${USER-}\" LOGNAME=\"${LOGNAME-}\" TMPDIR=\"${TMPDIR-}\" ",
        "CARGO_HOME=\"${CARGO_HOME-}\" RUSTUP_HOME=\"${RUSTUP_HOME-}\" ",
        "XDG_CONFIG_HOME=\"${XDG_CONFIG_HOME-}\" XDG_CACHE_HOME=\"${XDG_CACHE_HOME-}\" ",
        "LANG=\"${LANG-}\" LC_ALL=\"${LC_ALL-}\" \"$@\""
    );

    prefix.extend([
        "/bin/sh".into(),
        "-c".into(),
        CLEAN_ENV_SCRIPT.into(),
        "coding-tools-clean-env".into(),
        inner_program,
    ]);
    prefix.extend(inner_args);
    Some(WslInvocation {
        program: "wsl.exe".into(),
        args: prefix,
    })
}

#[cfg(test)]
mod tests {
    use super::{clean_env_invocation_for_path, invocation_for_path};
    use std::path::Path;

    #[test]
    fn builds_argument_safe_wsl_invocation() {
        if !cfg!(windows) {
            return;
        }
        let invocation = invocation_for_path(
            Path::new(r"\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project"),
            "cargo",
            &[
                "test".into(),
                r"\\wsl$\ubuntu-24.04\opt\src\Sample Project\Cargo.toml".into(),
                r"\\wsl.localhost\Debian\tmp\other".into(),
            ],
            &[("RUST_LOG".into(), "debug trace".into())],
            &["RUST_BACKTRACE".into()],
        )
        .expect("WSL invocation");
        assert_eq!(invocation.program, "wsl.exe");
        assert_eq!(
            invocation.args,
            vec![
                "--distribution",
                "Ubuntu-24.04",
                "--cd",
                "/opt/src/Sample Project",
                "--exec",
                "env",
                "-u",
                "RUST_BACKTRACE",
                "RUST_LOG=debug trace",
                "cargo",
                "test",
                "/opt/src/Sample Project/Cargo.toml",
                r"\\wsl.localhost\Debian\tmp\other"
            ]
        );
    }

    #[test]
    fn wsl_clean_environment_wrapper_preserves_argument_boundaries() {
        if !cfg!(windows) {
            return;
        }
        let invocation = clean_env_invocation_for_path(
            Path::new(r"\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project"),
            r"\\wsl.localhost\Ubuntu-24.04\opt\src\Sample Project\.venv\bin\ruff",
            &["format".into(), "file with spaces.py".into()],
        )
        .expect("clean WSL invocation");

        assert_eq!(invocation.program, "wsl.exe");
        assert_eq!(
            &invocation.args[..5],
            [
                "--distribution",
                "Ubuntu-24.04",
                "--cd",
                "/opt/src/Sample Project",
                "--exec",
            ]
        );
        assert_eq!(invocation.args[5], "/bin/sh");
        assert_eq!(invocation.args[6], "-c");
        assert!(invocation.args[7].contains("exec env -i"));
        assert_eq!(invocation.args[8], "coding-tools-clean-env");
        assert_eq!(
            &invocation.args[9..],
            [
                "/opt/src/Sample Project/.venv/bin/ruff",
                "format",
                "file with spaces.py",
            ]
        );
    }
}
