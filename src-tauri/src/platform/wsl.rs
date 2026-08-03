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

pub(crate) fn std_command_for_workspace(
    program: &str,
    args: &[String],
    cwd: &Path,
) -> std::process::Command {
    if let Some(invocation) = invocation_for_path(cwd, program, args, &[], &[]) {
        let mut command = std::process::Command::new(invocation.program);
        command.args(invocation.args);
        command
    } else {
        let mut command = std::process::Command::new(program);
        command.args(args).current_dir(cwd);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::invocation_for_path;
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
}
