use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::backend::{start_exec_process, CommandExecutionBackend};
use super::runner::CommandIoMode;
use super::spec::PostCheckSpec;

pub(super) async fn run_post_checks(
    post_checks: Vec<PostCheckSpec>,
    cwd: &Path,
    backend: &CommandExecutionBackend,
) -> Value {
    let configured = post_checks.len();
    let max_concurrency = configured.min(4).max(1);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(max_concurrency));
    let mut tasks = tokio::task::JoinSet::new();
    for (index, check) in post_checks.into_iter().enumerate() {
        let semaphore = semaphore.clone();
        let cwd = cwd.to_path_buf();
        let backend = (*backend).clone();
        tasks.spawn(async move {
            let _permit = semaphore.acquire_owned().await.ok();
            (index, run_post_check(&check, &cwd, &backend).await)
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
    json!({
        "ok": all_ok,
        "configured": configured,
        "executed": results.len(),
        "skipped": false,
        "execution_mode": "parallel",
        "max_concurrency": max_concurrency,
        "results": results
    })
}

async fn run_post_check(
    check: &PostCheckSpec,
    cwd: &Path,
    backend: &CommandExecutionBackend,
) -> Value {
    let start = Instant::now();
    let started = match tokio::time::timeout(
        check.timeout,
        start_exec_process(backend, &check.exec, cwd, CommandIoMode::PostCheck),
    )
    .await
    {
        Ok(Ok(started)) => started,
        Ok(Err(error)) => {
            let error_value = error.to_error_value();
            let startup = error_value
                .get("details")
                .and_then(|details| details.get("startup"))
                .cloned()
                .unwrap_or_else(|| json!({}));
            return json!({
                "name": check.name,
                "command": check.exec.display,
                "process_exit_code": null,
                "expected_exit_code": check.expected_exit_code,
                "passed": false,
                "timed_out": false,
                "stdout": "",
                "stderr": error.to_string(),
                "stdout_truncated": false,
                "stderr_truncated": false,
                "startup": startup,
                "startup_error": error_value,
                "duration_ms": start.elapsed().as_millis()
            });
        }
        Err(_) => {
            return json!({
                "name": check.name,
                "command": check.exec.display,
                "process_exit_code": null,
                "expected_exit_code": check.expected_exit_code,
                "passed": false,
                "timed_out": true,
                "stdout": "",
                "stderr": "post-check timed out during process startup",
                "stdout_truncated": false,
                "stderr_truncated": false,
                "duration_ms": start.elapsed().as_millis()
            });
        }
    };

    let diagnostics = started.diagnostics;
    let mut child = started.child;
    let remaining = check.timeout.saturating_sub(start.elapsed());
    if remaining.is_zero() {
        let cancellation_error = child.cancel().await.err().map(|error| error.to_string());
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        return json!({
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
            "startup": diagnostics.to_json(),
            "cancellation_error": cancellation_error,
            "duration_ms": start.elapsed().as_millis()
        });
    }

    match tokio::time::timeout(remaining, child.wait_with_output_mut()).await {
        Ok(Ok(output)) => {
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
            "stderr": error.to_string(),
            "stdout_truncated": false,
            "stderr_truncated": false,
            "startup": diagnostics.to_json(),
            "duration_ms": start.elapsed().as_millis()
        }),
        Err(_) => {
            let cancellation_error = child.cancel().await.err().map(|error| error.to_string());
            let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
            json!({
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
                "startup": diagnostics.to_json(),
                "cancellation_error": cancellation_error,
                "duration_ms": start.elapsed().as_millis()
            })
        }
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use super::*;
    use crate::tools::process_child::ProcessChild;
    use crate::tools::process_spec::ProcessLaunchSpec;
    use crate::tools::sandbox::{PreparedSandbox, SandboxCommand, SandboxProcessPlan};
    use crate::tools::workspace::WorkspaceResult;

    struct TimeoutBackend {
        cancellations: Arc<AtomicUsize>,
    }

    impl PreparedSandbox for TimeoutBackend {
        fn backend_id(&self) -> &str {
            "timeout-test"
        }

        fn prepare_command(
            &self,
            command: SandboxCommand,
            env: Vec<(String, String)>,
            remove_env: Vec<String>,
        ) -> WorkspaceResult<SandboxProcessPlan> {
            Ok(SandboxProcessPlan {
                backend_id: self.backend_id().to_string(),
                process: ProcessLaunchSpec {
                    program: command.executable,
                    args: command.args,
                    cwd: Some(command.cwd),
                    env,
                    remove_env,
                    required_env: Vec::new(),
                    windows_raw_arg: None,
                    using_wsl: false,
                },
                environment_overrides: Default::default(),
                state: None,
            })
        }

        fn launch_prepared_process(
            &self,
            _plan: SandboxProcessPlan,
        ) -> WorkspaceResult<ProcessChild> {
            #[cfg(windows)]
            let child = tokio::process::Command::new("cmd")
                .args(["/d", "/c", "ping 127.0.0.1 -n 6 >nul"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn timeout child");
            #[cfg(unix)]
            let child = tokio::process::Command::new("sh")
                .args(["-c", "sleep 5"])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn timeout child");

            let cancellations = Arc::clone(&self.cancellations);
            Ok(
                ProcessChild::from_tokio(child).with_kill_hook(Arc::new(move || {
                    cancellations.fetch_add(1, Ordering::AcqRel);
                    Ok(())
                })),
            )
        }
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn post_check_timeout_invokes_backend_cancellation() {
        let cancellations = Arc::new(AtomicUsize::new(0));
        let backend = CommandExecutionBackend::Sandbox(Arc::new(TimeoutBackend {
            cancellations: Arc::clone(&cancellations),
        }));
        let cwd = tempfile::tempdir().expect("cwd");
        let checks = vec![PostCheckSpec {
            name: "timeout".into(),
            exec: super::super::spec::ExecSpec {
                display: "timeout-test".into(),
                program: "ignored".into(),
                args: Vec::new(),
                shell: "none".into(),
                env: Vec::new(),
                remove_env: Vec::new(),
            },
            expected_exit_code: 0,
            timeout: Duration::from_millis(1_000),
            max_output_bytes: 1024,
        }];

        let result = run_post_checks(checks, cwd.path(), &backend).await;
        assert_eq!(result["ok"], false, "{result}");
        assert_eq!(result["results"][0]["timed_out"], true, "{result}");
        assert_eq!(cancellations.load(Ordering::Acquire), 1);
    }
}
