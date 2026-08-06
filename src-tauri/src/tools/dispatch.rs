use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::harness::model::OperationRecord;
use crate::tools::context::{MutationLockGroup, SharedToolContext, ToolContext};
use crate::tools::parallel_stats::{
    parallel_pair_history, parallel_safety_lower_bound, record_parallel_observations,
    ParallelPairStats,
};
use crate::tools::policy::{validate_tool_arguments_for_workspace, PolicyError};
use crate::tools::redaction::{redact_tool_output, OutputRedactionContext};
use crate::tools::workspace::{tool_err, tool_err_code, tool_ok, WorkspaceError};
use crate::tools::{exec, file, file_action, git, history, image_tool, patch, project, session};

const ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);
const PARALLEL_MIN_CONFIDENT_SAMPLES: u64 = 5;
const PARALLEL_SAFE_LOWER_BOUND: f64 = 0.70;
const MAX_PARALLEL_OBSERVATIONS: usize = 128;

fn policy_tool_err(
    ctx: &ToolContext,
    tool_name: &str,
    arguments: &Value,
    err: PolicyError,
) -> Value {
    let dangerous = err
        .0
        .strip_prefix("DANGEROUS_OPERATION_REQUIRES_CONFIRMATION: ");
    let protected = err.0.strip_prefix("PROTECTED_REPOSITORY_ASSET: ");
    let code = if protected.is_some() {
        "PROTECTED_REPOSITORY_ASSET"
    } else if dangerous.is_some() {
        "DANGEROUS_OPERATION_REQUIRES_CONFIRMATION"
    } else {
        "POLICY_REJECTED"
    };
    let message = protected.or(dangerous).unwrap_or(&err.0).to_string();
    let (reason, suggestion) = if dangerous.is_some() {
        (
            "confirmation_required",
            "为危险操作补充 confirm=true，确认后再重试",
        )
    } else if message.contains("allowlisted") {
        ("command_rejected", "改用允许的命令，或调整工作区命令白名单")
    } else if message.contains("Shell chaining") {
        (
            "shell_syntax_rejected",
            "移除未加引号的 shell 操作符；引号内的程序参数可以保留",
        )
    } else {
        ("policy_rejected", "根据错误信息修正参数后重试")
    };
    let permission = permission_kind(&message);
    let pending = permission.map(|permission| {
        ctx.pending_operations.insert(
            tool_name,
            arguments,
            permission,
            &message,
            Duration::from_secs(300),
        )
    });
    let recoverable = pending.is_some() || reason != "confirmation_required";
    tool_err(WorkspaceError::ToolDetails {
        code,
        message,
        category: "policy",
        retryable: false,
        details: json!({
            "stage": "policy",
            "reason": reason,
            "recoverable": recoverable,
            "suggestion": suggestion,
            "permission_request": pending.map(|operation| json!({
                "resume_id": operation.resume_id,
                "tool_name": operation.tool_name,
                "permission": operation.permission,
                "reason": operation.reason,
                "ttl_seconds": 300,
                "resume_with": "request_permissions"
            }))
        }),
    })
}

fn permission_kind(message: &str) -> Option<&'static str> {
    let lowered = message.to_ascii_lowercase();
    if lowered.contains("network") {
        Some("network")
    } else if lowered.contains("shell") {
        Some("shell_expansion")
    } else if lowered.contains("dangerous") || lowered.contains("confirmation") {
        Some("destructive_command")
    } else {
        None
    }
}

/// **唯一工具执行入口**。MCP `tools/call` 与 Actions `POST /actions/{tool}` 必须且只能调用此函数。
/// 策略校验、分发、错误格式在此统一，两路传输层不得另做执行前校验（Actions 仅允许额外的暴露层 `validate_actions_exposure`）。
pub fn call_tool(ctx: &ToolContext, name: &str, args: &Value) -> Value {
    redact_tool_output(name, args, call_tool_inner(ctx, name, args, false))
}

pub async fn call_tool_async(ctx: SharedToolContext, name: String, args: Value) -> Value {
    let redaction = OutputRedactionContext::new(&name, &args);
    let lock_groups = mutation_lock_groups(ctx.as_ref(), &name, &args);
    let lock_started = Instant::now();
    let mut mutation_guards = Vec::with_capacity(lock_groups.len());
    for group in &lock_groups {
        mutation_guards.push(ctx.mutation_lock_for(*group).lock_owned().await);
    }
    let workspace_lock_wait_ms = lock_started.elapsed().as_millis();
    let mut output = call_tool_async_inner(ctx, name, args).await;
    if let Some(object) = output.as_object_mut() {
        let lock_names = lock_groups
            .iter()
            .map(|group| group.as_str())
            .collect::<Vec<_>>();
        object.insert(
            "workspace_lock_scope".into(),
            json!(if lock_names.is_empty() {
                "none".to_string()
            } else {
                lock_names.join("+")
            }),
        );
        object.insert("workspace_lock_groups".into(), json!(lock_names));
        object.insert(
            "workspace_lock_wait_ms".into(),
            json!(workspace_lock_wait_ms),
        );
    }
    drop(mutation_guards);
    redaction.redact(output)
}

fn mutation_lock_groups(ctx: &ToolContext, name: &str, args: &Value) -> Vec<MutationLockGroup> {
    let effective_name = if name == "request_permissions" {
        args.get("resume_id")
            .and_then(Value::as_str)
            .and_then(|resume_id| ctx.pending_operations.tool_name(resume_id))
            .unwrap_or_else(|| name.to_string())
    } else {
        name.to_string()
    };
    let mut groups = match effective_name.as_str() {
        "history_session_bootstrap" | "history_session_checkpoint" | "history_session_validate" => {
            vec![MutationLockGroup::History]
        }
        "apply_patch" | "edit" | "edit_file" | "edit_many" | "file_ops" => {
            vec![MutationLockGroup::WorkspaceContent]
        }
        "git_restore" => vec![MutationLockGroup::WorkspaceContent, MutationLockGroup::Git],
        "git_branch" | "git_stage" | "git_commit" => vec![MutationLockGroup::Git],
        "start_task" | "update_task" | "pause_task" | "resume_task" | "finish_task" => {
            vec![MutationLockGroup::Task]
        }
        "set_default_cwd" => vec![MutationLockGroup::Cwd],
        _ => Vec::new(),
    };
    groups.sort_unstable();
    groups.dedup();
    groups
}

async fn call_tool_async_inner(ctx: SharedToolContext, name: String, args: Value) -> Value {
    if name == "exec_many" {
        return call_exec_many_async(ctx, &args).await;
    }
    if matches!(
        name.as_str(),
        "wait_command"
            | "resolve_operation"
            | "list_sessions"
            | "send_input"
            | "read_output"
            | "kill_session"
    ) {
        return call_session_tool_async(ctx.as_ref(), &name, &args).await;
    }

    let Some((
        admission_lane,
        admission_limit,
        admission,
        global_admission_limit,
        global_admission,
    )) = ctx.admission_for(&name)
    else {
        let mut value = call_tool(ctx.as_ref(), &name, &args);
        if let Some(object) = value.as_object_mut() {
            object.insert("execution_lane".into(), json!("inline_fast"));
            object.insert("blocking_queue_wait_ms".into(), json!(0));
            object.insert("admission_lane".into(), json!("fast"));
            object.insert("admission_limit".into(), json!(0));
            object.insert("global_admission_limit".into(), json!(0));
            object.insert("workspace_admission_wait_ms".into(), json!(0));
            object.insert("global_admission_wait_ms".into(), json!(0));
            object.insert("admission_queue_wait_ms".into(), json!(0));
        }
        return value;
    };

    let admission_started = Instant::now();
    let workspace_started = Instant::now();
    let permit = match tokio::time::timeout(ADMISSION_TIMEOUT, admission.acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(error)) => {
            return admission_error(
                admission_lane,
                "workspace",
                admission_limit,
                workspace_started.elapsed().as_millis(),
                0,
                format!("Workspace tool admission lane closed: {error}"),
            )
        }
        Err(_) => {
            return admission_error(
                admission_lane,
                "workspace",
                admission_limit,
                workspace_started.elapsed().as_millis(),
                0,
                "Workspace tool admission queue exceeded 30 seconds".into(),
            )
        }
    };
    let workspace_admission_wait_ms = workspace_started.elapsed().as_millis();
    let remaining = ADMISSION_TIMEOUT.saturating_sub(admission_started.elapsed());
    let global_started = Instant::now();
    let global_permit =
        match tokio::time::timeout(remaining, global_admission.acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => {
                return admission_error(
                    admission_lane,
                    "global",
                    global_admission_limit,
                    workspace_admission_wait_ms,
                    global_started.elapsed().as_millis(),
                    format!("Global tool admission lane closed: {error}"),
                )
            }
            Err(_) => {
                return admission_error(
                    admission_lane,
                    "global",
                    global_admission_limit,
                    workspace_admission_wait_ms,
                    global_started.elapsed().as_millis(),
                    "Combined workspace/global admission queue exceeded 30 seconds".into(),
                )
            }
        };
    let global_admission_wait_ms = global_started.elapsed().as_millis();
    let admission_queue_wait_ms = admission_started.elapsed().as_millis();

    if name == "exec_command" {
        let _global_permit = global_permit;
        let _permit = permit;
        let mut value = call_exec_tool_async(ctx.as_ref(), &name, &args).await;
        if let Some(object) = value.as_object_mut() {
            object.insert("execution_lane".into(), json!("async_process"));
            object.insert("blocking_queue_wait_ms".into(), json!(0));
            attach_admission_metadata(
                object,
                admission_lane,
                admission_limit,
                global_admission_limit,
                workspace_admission_wait_ms,
                global_admission_wait_ms,
                admission_queue_wait_ms,
            );
        }
        return value;
    }

    if name == "request_permissions" {
        let _global_permit = global_permit;
        let _permit = permit;
        let mut value = call_permission_tool_async(ctx.clone(), &name, &args).await;
        if let Some(object) = value.as_object_mut() {
            object.insert("execution_lane".into(), json!("async_permission"));
            object.insert("blocking_queue_wait_ms".into(), json!(0));
            attach_admission_metadata(
                object,
                admission_lane,
                admission_limit,
                global_admission_limit,
                workspace_admission_wait_ms,
                global_admission_wait_ms,
                admission_queue_wait_ms,
            );
        }
        return value;
    }

    let queued_at = Instant::now();
    let result = tokio::task::spawn_blocking(move || {
        let _global_permit = global_permit;
        let _permit = permit;
        let queue_wait_ms = queued_at.elapsed().as_millis();
        let mut value = call_tool(ctx.as_ref(), &name, &args);
        if let Some(object) = value.as_object_mut() {
            object.insert("execution_lane".into(), json!("blocking_worker"));
            object.insert("blocking_queue_wait_ms".into(), json!(queue_wait_ms));
            attach_admission_metadata(
                object,
                admission_lane,
                admission_limit,
                global_admission_limit,
                workspace_admission_wait_ms,
                global_admission_wait_ms,
                admission_queue_wait_ms,
            );
        }
        value
    })
    .await;
    match result {
        Ok(value) => value,
        Err(error) => {
            let mut value = tool_err(WorkspaceError::ToolDetails {
                code: "TOOL_WORKER_FAILED",
                message: format!("Tool worker failed: {error}"),
                category: "runtime",
                retryable: true,
                details: json!({
                    "stage": "tool_worker",
                    "reason": "join_failed",
                    "suggestion": "重试请求或重启 MCP 运行时"
                }),
            });
            if let Some(object) = value.as_object_mut() {
                object.insert("execution_lane".into(), json!("blocking_worker"));
                object.insert("blocking_queue_wait_ms".into(), json!(0));
                attach_admission_metadata(
                    object,
                    admission_lane,
                    admission_limit,
                    global_admission_limit,
                    workspace_admission_wait_ms,
                    global_admission_wait_ms,
                    admission_queue_wait_ms,
                );
            }
            value
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ParallelPrior {
    Safe,
    EvidenceRequired,
    Unsafe,
}

#[derive(Clone, Debug)]
struct ParallelDecision {
    mode: &'static str,
    source: &'static str,
    confidence: f64,
    history_samples: u64,
    blocked_pairs: usize,
    recommended_max_parallel: usize,
    reasons: Vec<String>,
}

#[derive(Clone)]
struct ExecBatchCommand {
    index: usize,
    id: String,
    depends_on: Vec<String>,
    lock_group: Option<String>,
    lock_group_inferred: bool,
    parallel_signature: String,
    parallel_prior: ParallelPrior,
    args: Value,
}

async fn call_exec_many_async(ctx: SharedToolContext, args: &Value) -> Value {
    let Some(commands) = args.get("commands").and_then(Value::as_array) else {
        return tool_err(WorkspaceError::invalid_argument("commands is required"));
    };
    let commands = match parse_exec_batch_commands(commands) {
        Ok(commands) => commands,
        Err(error) => return tool_err(error),
    };
    let requested_mode = match exec_many_mode(args) {
        Ok(mode) => mode,
        Err(error) => return tool_err(error),
    };
    let stop_on_error = args
        .get("stop_on_error")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let process_limit = ctx.execution_limits().process_admission.clamp(1, 256);
    let default_parallel = default_exec_many_parallelism(commands.len(), process_limit);
    let decision =
        resolve_exec_many_decision(&ctx.profile_id, requested_mode, &commands, default_parallel);
    let mode = decision.mode;
    if mode == "dag" {
        if let Err(error) = validate_exec_batch_dag(&commands) {
            return tool_err(error);
        }
    }
    let configured_parallel = args
        .get("max_parallel")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(default_parallel)
        .clamp(1, process_limit);
    let max_parallel = match mode {
        "sequential" => 1,
        _ if requested_mode == "auto" => configured_parallel
            .min(decision.recommended_max_parallel)
            .max(1),
        _ => configured_parallel,
    };
    let inferred_lock_groups = commands
        .iter()
        .filter(|command| command.lock_group_inferred)
        .count();
    let started = Instant::now();
    let mut warnings = Vec::<String>::new();
    if requested_mode == "auto" {
        warnings.push(format!(
            "auto scheduler selected {mode}; source={}; confidence={:.3}; samples={}; max_parallel={max_parallel}; inferred_lock_groups={inferred_lock_groups}",
            decision.source,
            decision.confidence,
            decision.history_samples,
        ));
        warnings.extend(decision.reasons.iter().take(8).cloned());
    }

    let results = match mode {
        "sequential" => {
            let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
            let mut results = Vec::with_capacity(commands.len());
            for command in commands.iter().cloned() {
                let result = run_exec_batch_command(ctx.clone(), command, semaphore.clone()).await;
                let command_ok = result.get("command_ok").and_then(Value::as_bool) == Some(true);
                results.push(result);
                if stop_on_error && !command_ok {
                    break;
                }
            }
            results
        }
        "parallel" => {
            if stop_on_error {
                warnings.push(
                    "parallel mode schedules independent commands immediately; stop_on_error cannot cancel commands that already started".into(),
                );
            }
            run_exec_batch_wave(ctx.clone(), commands.clone(), max_parallel).await
        }
        "dag" => {
            run_exec_batch_dag(ctx.clone(), commands.clone(), max_parallel, stop_on_error).await
        }
        _ => unreachable!("validated exec_many mode"),
    };

    let (parallelism_observations, observations_truncated) =
        collect_parallelism_observations(&commands, &results, mode);
    record_parallel_observations(&ctx.profile_id, &parallelism_observations);
    let observation_count = parallelism_observations.len();
    let mut output = exec_many_output(
        ctx.as_ref(),
        mode,
        max_parallel,
        stop_on_error,
        commands.len(),
        results,
        started,
        warnings,
        "async_batch",
    );
    if let Some(object) = output.as_object_mut() {
        object.insert("requested_mode".into(), json!(requested_mode));
        object.insert("auto_selected".into(), json!(requested_mode == "auto"));
        object.insert(
            "inferred_lock_group_count".into(),
            json!(inferred_lock_groups),
        );
        object.insert("parallel_decision_source".into(), json!(decision.source));
        object.insert(
            "parallel_confidence".into(),
            json!(round_parallel_confidence(decision.confidence)),
        );
        object.insert(
            "parallel_history_samples".into(),
            json!(decision.history_samples),
        );
        object.insert(
            "parallel_blocked_pair_count".into(),
            json!(decision.blocked_pairs),
        );
        object.insert(
            "recommended_max_parallel".into(),
            json!(decision.recommended_max_parallel),
        );
        object.insert("parallel_decision_reasons".into(), json!(decision.reasons));
        object.insert(
            "parallel_observation_count".into(),
            json!(observation_count),
        );
        object.insert(
            "parallelism_observation_truncated".into(),
            json!(observations_truncated),
        );
        object.insert(
            "parallelism_observations".into(),
            Value::Array(parallelism_observations),
        );
    }
    output
}

fn call_exec_many_sync(ctx: &ToolContext, args: &Value) -> Value {
    let requested_mode = match exec_many_mode(args) {
        Ok(mode) => mode,
        Err(error) => return tool_err(error),
    };
    if !matches!(requested_mode, "auto" | "sequential") {
        return tool_err(WorkspaceError::invalid_argument(
            "parallel and dag exec_many modes require the async MCP/Actions execution path",
        ));
    }
    let mode = "sequential";
    let Some(commands) = args.get("commands").and_then(Value::as_array) else {
        return tool_err(WorkspaceError::invalid_argument("commands is required"));
    };
    let commands = match parse_exec_batch_commands(commands) {
        Ok(commands) => commands,
        Err(error) => return tool_err(error),
    };
    let stop_on_error = args
        .get("stop_on_error")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let started = Instant::now();
    let mut results = Vec::with_capacity(commands.len());
    for command in commands.iter().cloned() {
        let mut result = call_tool_inner(ctx, "exec_command", &command.args, false);
        while result.get("process_still_running").and_then(Value::as_bool) == Some(true) {
            let Some(session_id) = result
                .get("session_id")
                .and_then(Value::as_str)
                .map(str::to_string)
            else {
                break;
            };
            let cursor = result
                .get("next_cursor")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            result = match session::wait_command(
                &ctx.sessions,
                &json!({
                    "session_id": session_id,
                    "cursor": cursor,
                    "timeout_ms": 30000,
                    "until": "finalized",
                    "output_mode": "tail"
                }),
            ) {
                Ok(value) => value,
                Err(error) => tool_err(error),
            };
        }
        let command_ok = result.get("command_ok").and_then(Value::as_bool) == Some(true);
        results.push(batch_result(
            &command, result, command_ok, false, None, 0, 0,
        ));
        if stop_on_error && !command_ok {
            break;
        }
    }
    let mut output = exec_many_output(
        ctx,
        mode,
        1,
        stop_on_error,
        commands.len(),
        results,
        started,
        if requested_mode == "auto" {
            vec!["auto scheduler fell back to sequential on the blocking execution path".into()]
        } else {
            Vec::new()
        },
        "blocking_batch",
    );
    if let Some(object) = output.as_object_mut() {
        object.insert("requested_mode".into(), json!(requested_mode));
        object.insert("auto_selected".into(), json!(requested_mode == "auto"));
        object.insert(
            "parallel_decision_source".into(),
            json!("blocking_execution_path"),
        );
        object.insert("parallel_confidence".into(), json!(1.0));
        object.insert("parallel_history_samples".into(), json!(0));
        object.insert("parallel_blocked_pair_count".into(), json!(0));
        object.insert("recommended_max_parallel".into(), json!(1));
        object.insert(
            "parallel_decision_reasons".into(),
            json!(["The blocking execution path only supports sequential exec_many scheduling."]),
        );
        object.insert("parallel_observation_count".into(), json!(0));
        object.insert("parallelism_observation_truncated".into(), json!(false));
        object.insert("parallelism_observations".into(), json!([]));
    }
    output
}

fn exec_many_mode(args: &Value) -> Result<&str, WorkspaceError> {
    let mode = args.get("mode").and_then(Value::as_str).unwrap_or("auto");
    if matches!(mode, "auto" | "sequential" | "parallel" | "dag") {
        Ok(mode)
    } else {
        Err(WorkspaceError::invalid_argument(
            "mode must be auto, sequential, parallel, or dag",
        ))
    }
}

fn default_exec_many_parallelism(command_count: usize, process_limit: usize) -> usize {
    command_count.min(process_limit).clamp(1, 8)
}

fn resolve_exec_many_decision(
    profile_id: &str,
    requested: &str,
    commands: &[ExecBatchCommand],
    default_parallel: usize,
) -> ParallelDecision {
    let pairs = parallel_command_pairs(commands);
    let pair_keys = pairs
        .iter()
        .map(|(pair, _)| pair.clone())
        .collect::<Vec<_>>();
    let history = parallel_pair_history(profile_id, &pair_keys);
    resolve_exec_many_decision_with_history(requested, commands, default_parallel, &history)
}

fn resolve_exec_many_decision_with_history(
    requested: &str,
    commands: &[ExecBatchCommand],
    default_parallel: usize,
    history: &BTreeMap<String, ParallelPairStats>,
) -> ParallelDecision {
    if requested != "auto" {
        let mode = match requested {
            "parallel" => "parallel",
            "dag" => "dag",
            _ => "sequential",
        };
        return ParallelDecision {
            mode,
            source: "explicit",
            confidence: 1.0,
            history_samples: 0,
            blocked_pairs: 0,
            recommended_max_parallel: if mode == "sequential" {
                1
            } else {
                default_parallel
            },
            reasons: vec!["The caller explicitly selected the execution mode.".into()],
        };
    }
    if commands
        .iter()
        .any(|command| !command.depends_on.is_empty())
    {
        return ParallelDecision {
            mode: "dag",
            source: "dependency_graph",
            confidence: 1.0,
            history_samples: 0,
            blocked_pairs: 0,
            recommended_max_parallel: default_parallel,
            reasons: vec!["Command dependencies require DAG scheduling.".into()],
        };
    }
    if commands.len() <= 1 {
        return ParallelDecision {
            mode: "sequential",
            source: "single_command",
            confidence: 1.0,
            history_samples: 0,
            blocked_pairs: 0,
            recommended_max_parallel: 1,
            reasons: vec!["Only one command was supplied.".into()],
        };
    }
    let unsafe_commands = commands
        .iter()
        .filter(|command| command.parallel_prior == ParallelPrior::Unsafe)
        .count();
    if unsafe_commands > 0 {
        return ParallelDecision {
            mode: "sequential",
            source: "hard_safety_rule",
            confidence: 1.0,
            history_samples: 0,
            blocked_pairs: 0,
            recommended_max_parallel: 1,
            reasons: vec![format!(
                "{unsafe_commands} opaque or shell command(s) cannot be proven safe for automatic parallel execution."
            )],
        };
    }

    let pairs = parallel_command_pairs(commands);
    let mut confidence = 0.75_f64;
    let mut history_samples = 0_u64;
    let mut blocked_pairs = 0_usize;
    let mut conflict_blocks = 0_usize;
    let mut serialization_pairs = 0_usize;
    let mut reasons = Vec::<String>::new();

    for (pair, requires_evidence) in &pairs {
        let stats = history.get(pair);
        let mut pair_blocked = false;
        if let Some(stats) = stats {
            let samples = stats.safety_samples();
            history_samples = history_samples.saturating_add(samples);
            if samples > 0 {
                confidence = confidence.min(parallel_safety_lower_bound(stats));
            }
            let conflict_prone =
                stats.conflicts >= 2 || (samples >= 3 && stats.conflict_rate() >= 0.10);
            if conflict_prone {
                conflict_blocks += 1;
                pair_blocked = true;
                reasons.push(format!(
                    "Historical conflicts block pair {pair}: conflicts={}, safety_samples={}.",
                    stats.conflicts, samples
                ));
            }
            if stats.attempts >= 3 && stats.serialization_rate() >= 0.60 {
                serialization_pairs += 1;
            }
            if *requires_evidence
                && (samples < PARALLEL_MIN_CONFIDENT_SAMPLES
                    || parallel_safety_lower_bound(stats) < PARALLEL_SAFE_LOWER_BOUND)
            {
                pair_blocked = true;
                reasons.push(format!(
                    "Pair {pair} lacks sufficient safe evidence: samples={samples}, lower_bound={:.3}.",
                    parallel_safety_lower_bound(stats)
                ));
            }
        } else if *requires_evidence {
            pair_blocked = true;
            confidence = 0.0;
            reasons.push(format!(
                "Pair {pair} has no parallel safety history; run it explicitly in parallel to collect evidence."
            ));
        }
        if pair_blocked {
            blocked_pairs += 1;
        }
    }

    if blocked_pairs > 0 {
        return ParallelDecision {
            mode: "sequential",
            source: if conflict_blocks > 0 {
                "historical_conflict"
            } else {
                "insufficient_history"
            },
            confidence,
            history_samples,
            blocked_pairs,
            recommended_max_parallel: 1,
            reasons,
        };
    }
    if !pairs.is_empty() && serialization_pairs == pairs.len() {
        reasons.push(
            "All observed command pairs mostly serialize on resource locks; parallel dispatch adds no useful overlap."
                .into(),
        );
        return ParallelDecision {
            mode: "sequential",
            source: "historical_serialization",
            confidence,
            history_samples,
            blocked_pairs: 0,
            recommended_max_parallel: 1,
            reasons,
        };
    }

    let recommended_max_parallel = if serialization_pairs > 0 {
        reasons.push(format!(
            "{serialization_pairs} pair(s) usually serialize on resource locks; cap automatic parallelism at 2."
        ));
        default_parallel.min(2)
    } else {
        default_parallel
    };
    if history_samples == 0 {
        reasons.push(
            "Known safe command families and hard resource locks permit parallel execution.".into(),
        );
    } else {
        reasons.push(format!(
            "Historical evidence supports parallel execution with an 80% Wilson lower bound of {:.3}.",
            confidence
        ));
    }
    ParallelDecision {
        mode: "parallel",
        source: if history_samples == 0 {
            "hard_rules"
        } else {
            "historical_statistics"
        },
        confidence,
        history_samples,
        blocked_pairs: 0,
        recommended_max_parallel,
        reasons,
    }
}

fn parallel_command_pairs(commands: &[ExecBatchCommand]) -> Vec<(String, bool)> {
    let mut pairs = Vec::new();
    for left in 0..commands.len() {
        for right in (left + 1)..commands.len() {
            let requires_evidence = commands[left].parallel_prior
                == ParallelPrior::EvidenceRequired
                || commands[right].parallel_prior == ParallelPrior::EvidenceRequired;
            pairs.push((
                parallel_pair_key(
                    &commands[left].parallel_signature,
                    &commands[right].parallel_signature,
                ),
                requires_evidence,
            ));
        }
    }
    pairs
}

fn parallel_pair_key(left: &str, right: &str) -> String {
    if left <= right {
        format!("{left}|{right}")
    } else {
        format!("{right}|{left}")
    }
}

fn command_parallel_prior(arguments: &Value, lock_group: Option<&str>) -> ParallelPrior {
    let Some(program) = normalized_program(arguments) else {
        return ParallelPrior::Unsafe;
    };
    if arguments
        .get("shell")
        .and_then(Value::as_str)
        .is_some_and(|shell| shell != "none")
    {
        return ParallelPrior::Unsafe;
    }
    if lock_group.is_some() || command_has_version_flag(arguments) {
        return ParallelPrior::Safe;
    }
    let verb = normalized_command_verb(arguments, &program);
    if program == "git"
        && matches!(
            verb.as_str(),
            "status" | "diff" | "log" | "show" | "rev-parse" | "ls-files" | "cat-file"
        )
    {
        ParallelPrior::Safe
    } else {
        ParallelPrior::EvidenceRequired
    }
}

fn command_parallel_signature(arguments: &Value) -> String {
    let program = normalized_program(arguments).unwrap_or_else(|| "opaque".into());
    let verb = normalized_command_verb(arguments, &program);
    let workdir = arguments
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let digest = format!("{:x}", Sha256::digest(workdir.as_bytes()));
    format!("{program}:{verb}@{}", &digest[..12])
}

fn normalized_program(arguments: &Value) -> Option<String> {
    let value = arguments
        .get("program")
        .and_then(Value::as_str)
        .and_then(|program| Path::new(program).file_stem())
        .and_then(|name| name.to_str())?;
    let normalized = value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if matches!(
        normalized.as_str(),
        "cargo"
            | "rustc"
            | "git"
            | "npm"
            | "pnpm"
            | "yarn"
            | "bun"
            | "python"
            | "python3"
            | "node"
            | "pwsh"
            | "powershell"
            | "sh"
            | "cmd"
    ) {
        return Some(normalized);
    }
    let digest = format!("{:x}", Sha256::digest(normalized.as_bytes()));
    Some(format!("custom-{}", &digest[..12]))
}

fn normalized_command_verb(arguments: &Value, program: &str) -> String {
    if command_has_version_flag(arguments) {
        return "version".into();
    }
    let first = arguments
        .get("args")
        .and_then(Value::as_array)
        .and_then(|args| args.iter().find_map(Value::as_str))
        .unwrap_or("");
    let known_family = matches!(
        program,
        "cargo" | "rustc" | "git" | "npm" | "pnpm" | "yarn" | "bun"
    );
    if !known_family {
        return if matches!(
            program,
            "python" | "python3" | "node" | "pwsh" | "powershell" | "sh" | "cmd"
        ) {
            "script".into()
        } else {
            "default".into()
        };
    }
    let normalized = first
        .trim_start_matches('-')
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase();
    if normalized.is_empty() {
        "default".into()
    } else {
        normalized
    }
}

fn command_has_version_flag(arguments: &Value) -> bool {
    arguments
        .get("args")
        .and_then(Value::as_array)
        .is_some_and(|args| {
            args.iter()
                .filter_map(Value::as_str)
                .any(|argument| matches!(argument, "--version" | "-V" | "-v" | "version"))
        })
}

fn round_parallel_confidence(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}
fn collect_parallelism_observations(
    commands: &[ExecBatchCommand],
    results: &[Value],
    mode: &str,
) -> (Vec<Value>, bool) {
    if mode != "parallel" || commands.len() < 2 {
        return (Vec::new(), false);
    }
    let result_by_id = results
        .iter()
        .filter_map(|result| {
            result
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id, result))
        })
        .collect::<HashMap<_, _>>();
    let total_pairs = commands
        .len()
        .saturating_mul(commands.len().saturating_sub(1))
        / 2;
    let mut observations = Vec::with_capacity(total_pairs.min(MAX_PARALLEL_OBSERVATIONS));
    for left in 0..commands.len() {
        for right in (left + 1)..commands.len() {
            if observations.len() == MAX_PARALLEL_OBSERVATIONS {
                return (observations, true);
            }
            let Some(left_result) = result_by_id.get(commands[left].id.as_str()) else {
                continue;
            };
            let Some(right_result) = result_by_id.get(commands[right].id.as_str()) else {
                continue;
            };
            let overlap_ms = execution_overlap_ms(left_result, right_result);
            let lock_wait_ms =
                result_lock_wait_ms(left_result).saturating_add(result_lock_wait_ms(right_result));
            let same_lock_group = commands[left].lock_group.is_some()
                && commands[left].lock_group == commands[right].lock_group;
            let left_ok = left_result.get("command_ok").and_then(Value::as_bool) == Some(true);
            let right_ok = right_result.get("command_ok").and_then(Value::as_bool) == Some(true);
            let outcome = if overlap_ms == 0 && (same_lock_group || lock_wait_ms > 0) {
                "serialized"
            } else if overlap_ms > 0
                && (result_has_conflict_marker(left_result)
                    || result_has_conflict_marker(right_result))
            {
                "conflict"
            } else if overlap_ms > 0 && left_ok && right_ok {
                "success"
            } else if overlap_ms > 0 {
                "failure"
            } else {
                "not_overlapped"
            };
            observations.push(json!({
                "pair": parallel_pair_key(
                    &commands[left].parallel_signature,
                    &commands[right].parallel_signature,
                ),
                "left": commands[left].parallel_signature,
                "right": commands[right].parallel_signature,
                "outcome": outcome,
                "overlap_ms": overlap_ms,
                "lock_wait_ms": lock_wait_ms,
                "same_lock_group": same_lock_group
            }));
        }
    }
    (observations, total_pairs > MAX_PARALLEL_OBSERVATIONS)
}

fn execution_overlap_ms(left: &Value, right: &Value) -> u64 {
    let Some((left_start, left_end)) = execution_interval(left) else {
        return 0;
    };
    let Some((right_start, right_end)) = execution_interval(right) else {
        return 0;
    };
    left_end
        .min(right_end)
        .saturating_sub(left_start.max(right_start))
}

fn execution_interval(batch_result: &Value) -> Option<(u64, u64)> {
    let result = batch_result.get("result")?;
    let start = result.get("started_ts_ms").and_then(Value::as_u64)?;
    let end = result
        .get("completed_ts_ms")
        .and_then(Value::as_u64)
        .or_else(|| {
            result
                .get("elapsed_ms")
                .or_else(|| result.get("duration_ms"))
                .and_then(Value::as_u64)
                .map(|duration| start.saturating_add(duration))
        })?;
    Some((start, end.max(start)))
}

fn result_lock_wait_ms(batch_result: &Value) -> u64 {
    batch_result
        .get("resource_lock_wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0)
}

fn result_has_conflict_marker(batch_result: &Value) -> bool {
    let text = serde_json::to_string(batch_result.get("result").unwrap_or(&Value::Null))
        .unwrap_or_default()
        .to_ascii_lowercase();
    [
        "resource busy",
        "file in use",
        "sharing violation",
        "index.lock",
        "another git process",
        "could not lock",
        "database is locked",
        "lock wait timeout",
        "text file busy",
        "ebusy",
    ]
    .iter()
    .any(|marker| text.contains(marker))
}

fn inferred_exec_lock_group(arguments: &Value) -> Option<String> {
    let program = arguments
        .get("program")
        .and_then(Value::as_str)
        .and_then(|program| Path::new(program).file_stem())
        .and_then(|name| name.to_str())?
        .to_ascii_lowercase();
    let verb = arguments
        .get("args")
        .and_then(Value::as_array)
        .and_then(|args| args.iter().find_map(Value::as_str))
        .unwrap_or("")
        .trim_start_matches('-')
        .to_ascii_lowercase();
    let workdir = arguments
        .get("workdir")
        .and_then(Value::as_str)
        .unwrap_or(".");
    let scope = workdir
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '_'
            }
        })
        .take(80)
        .collect::<String>();
    let group = match program.as_str() {
        "cargo" | "rustc" => format!("cargo-target:{scope}"),
        "git"
            if matches!(
                verb.as_str(),
                "add"
                    | "am"
                    | "apply"
                    | "bisect"
                    | "branch"
                    | "checkout"
                    | "cherry-pick"
                    | "clean"
                    | "commit"
                    | "merge"
                    | "mv"
                    | "rebase"
                    | "reset"
                    | "restore"
                    | "revert"
                    | "rm"
                    | "stash"
                    | "switch"
                    | "tag"
            ) =>
        {
            format!("git-index:{scope}")
        }
        "npm" | "pnpm" | "yarn" | "bun"
            if matches!(
                verb.as_str(),
                "add" | "ci" | "dedupe" | "install" | "remove" | "uninstall" | "update"
            ) =>
        {
            format!("node-generated:{scope}")
        }
        _ => return None,
    };
    Some(group)
}

fn parse_exec_batch_commands(commands: &[Value]) -> Result<Vec<ExecBatchCommand>, WorkspaceError> {
    if commands.is_empty() || commands.len() > 256 {
        return Err(WorkspaceError::invalid_argument(
            "commands must contain between 1 and 256 entries",
        ));
    }
    let mut parsed = Vec::with_capacity(commands.len());
    let mut ids = HashSet::with_capacity(commands.len());
    for (index, command) in commands.iter().enumerate() {
        let Some(_) = command.as_object() else {
            return Err(WorkspaceError::invalid_argument(format!(
                "commands[{index}] must be an object"
            )));
        };
        let mut command_args = command.clone();
        let object = command_args
            .as_object_mut()
            .expect("validated command object");
        let id = match object.remove("id") {
            None => format!("command-{index}"),
            Some(Value::String(value)) => value,
            Some(_) => {
                return Err(WorkspaceError::invalid_argument(format!(
                    "commands[{index}].id must be a string"
                )))
            }
        };
        if id.trim().is_empty() || id.len() > 128 {
            return Err(WorkspaceError::invalid_argument(format!(
                "commands[{index}].id must contain between 1 and 128 bytes"
            )));
        }
        if !ids.insert(id.clone()) {
            return Err(WorkspaceError::invalid_argument(format!(
                "duplicate exec_many command id: {id}"
            )));
        }
        let depends_on = match object.remove("depends_on") {
            None => Vec::new(),
            Some(Value::Array(values)) => values
                .into_iter()
                .map(|value| {
                    value.as_str().map(str::to_string).ok_or_else(|| {
                        WorkspaceError::invalid_argument(format!(
                            "commands[{index}].depends_on must contain strings"
                        ))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => {
                return Err(WorkspaceError::invalid_argument(format!(
                    "commands[{index}].depends_on must be an array"
                )))
            }
        };
        let explicit_lock_group = match object.remove("lock_group") {
            None => None,
            Some(Value::String(value)) => {
                let value = value.trim().to_string();
                if value.is_empty() || value.len() > 128 {
                    return Err(WorkspaceError::invalid_argument(format!(
                        "commands[{index}].lock_group must contain between 1 and 128 bytes"
                    )));
                }
                Some(value)
            }
            Some(_) => {
                return Err(WorkspaceError::invalid_argument(format!(
                    "commands[{index}].lock_group must be a string"
                )))
            }
        };
        let (lock_group, lock_group_inferred) = match explicit_lock_group {
            Some(group) => (Some(group), false),
            None => (
                inferred_exec_lock_group(&Value::Object(object.clone())),
                true,
            ),
        };
        let lock_group_inferred = lock_group_inferred && lock_group.is_some();
        if let Some(group) = lock_group.as_ref() {
            object.insert("lock_group".into(), Value::String(group.clone()));
        }
        if command_args.get("yield_time_ms").is_none() {
            command_args["yield_time_ms"] = json!(30_000);
        }
        if command_args.get("output_mode").is_none() {
            command_args["output_mode"] = json!("tail");
        }
        let parallel_signature = command_parallel_signature(&command_args);
        let parallel_prior = command_parallel_prior(&command_args, lock_group.as_deref());
        parsed.push(ExecBatchCommand {
            index,
            id,
            depends_on,
            lock_group,
            lock_group_inferred,
            parallel_signature,
            parallel_prior,
            args: command_args,
        });
    }
    let known_ids = parsed
        .iter()
        .map(|command| command.id.as_str())
        .collect::<HashSet<_>>();
    for command in &parsed {
        for dependency in &command.depends_on {
            if dependency == &command.id {
                return Err(WorkspaceError::invalid_argument(format!(
                    "command {} cannot depend on itself",
                    command.id
                )));
            }
            if !known_ids.contains(dependency.as_str()) {
                return Err(WorkspaceError::invalid_argument(format!(
                    "command {} depends on unknown command {}",
                    command.id, dependency
                )));
            }
        }
    }
    Ok(parsed)
}

fn validate_exec_batch_dag(commands: &[ExecBatchCommand]) -> Result<(), WorkspaceError> {
    let mut indegree = commands
        .iter()
        .map(|command| (command.id.clone(), command.depends_on.len()))
        .collect::<HashMap<_, _>>();
    let mut dependents = HashMap::<String, Vec<String>>::new();
    for command in commands {
        for dependency in &command.depends_on {
            dependents
                .entry(dependency.clone())
                .or_default()
                .push(command.id.clone());
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(id.clone()))
        .collect::<Vec<_>>();
    let mut visited = 0_usize;
    while let Some(id) = ready.pop() {
        visited += 1;
        for dependent in dependents.get(&id).into_iter().flatten() {
            let degree = indegree
                .get_mut(dependent)
                .expect("validated dependent command");
            *degree = degree.saturating_sub(1);
            if *degree == 0 {
                ready.push(dependent.clone());
            }
        }
    }
    if visited == commands.len() {
        Ok(())
    } else {
        Err(WorkspaceError::invalid_argument(
            "exec_many DAG contains a dependency cycle",
        ))
    }
}

async fn run_exec_batch_dag(
    ctx: SharedToolContext,
    commands: Vec<ExecBatchCommand>,
    max_parallel: usize,
    stop_on_error: bool,
) -> Vec<Value> {
    let mut pending = commands;
    let mut completed = HashMap::<String, bool>::new();
    let mut results = Vec::new();
    while !pending.is_empty() {
        let mut ready = Vec::new();
        let mut remaining = Vec::new();
        for command in pending {
            if command
                .depends_on
                .iter()
                .any(|dependency| completed.get(dependency) == Some(&false))
            {
                completed.insert(command.id.clone(), false);
                results.push(skipped_batch_result(command, "dependency_failed"));
            } else if command
                .depends_on
                .iter()
                .all(|dependency| completed.get(dependency) == Some(&true))
            {
                ready.push(command);
            } else {
                remaining.push(command);
            }
        }
        if ready.is_empty() {
            if remaining.is_empty() {
                break;
            }
            for command in remaining {
                completed.insert(command.id.clone(), false);
                results.push(skipped_batch_result(command, "dependency_unresolved"));
            }
            break;
        }
        let wave_results = run_exec_batch_wave(ctx.clone(), ready, max_parallel).await;
        let wave_failed = wave_results
            .iter()
            .any(|result| result.get("command_ok").and_then(Value::as_bool) != Some(true));
        for result in &wave_results {
            if let Some(id) = result.get("id").and_then(Value::as_str) {
                completed.insert(
                    id.to_string(),
                    result.get("command_ok").and_then(Value::as_bool) == Some(true),
                );
            }
        }
        results.extend(wave_results);
        if stop_on_error && wave_failed {
            for command in remaining {
                completed.insert(command.id.clone(), false);
                results.push(skipped_batch_result(command, "stopped_after_failure"));
            }
            break;
        }
        pending = remaining;
    }
    results.sort_by_key(|result| {
        result
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    });
    results
}

async fn run_exec_batch_wave(
    ctx: SharedToolContext,
    commands: Vec<ExecBatchCommand>,
    max_parallel: usize,
) -> Vec<Value> {
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_parallel.max(1)));
    let mut tasks = tokio::task::JoinSet::new();
    for command in commands {
        let ctx = ctx.clone();
        let semaphore = semaphore.clone();
        tasks.spawn(async move {
            let index = command.index;
            (index, run_exec_batch_command(ctx, command, semaphore).await)
        });
    }
    let mut results = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((_, result)) => results.push(result),
            Err(error) => results.push(json!({
                "index": u64::MAX,
                "id": "join-failure",
                "command_ok": false,
                "skipped": false,
                "result": tool_err(WorkspaceError::ToolDetails {
                    code: "BATCH_WORKER_FAILED",
                    message: error.to_string(),
                    category: "runtime",
                    retryable: true,
                    details: json!({"stage": "exec_many_join"})
                })
            })),
        }
    }
    results.sort_by_key(|result| {
        result
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    });
    results
}

async fn run_exec_batch_command(
    ctx: SharedToolContext,
    command: ExecBatchCommand,
    semaphore: Arc<tokio::sync::Semaphore>,
) -> Value {
    let resource_lock_wait_ms = 0u128;
    let batch_started = Instant::now();
    let batch_permit = semaphore
        .acquire_owned()
        .await
        .expect("exec_many semaphore closed");
    let batch_queue_wait_ms = batch_started.elapsed().as_millis();
    let redaction = OutputRedactionContext::new("exec_command", &command.args);
    let Some((
        admission_lane,
        admission_limit,
        admission,
        global_admission_limit,
        global_admission,
    )) = ctx.admission_for("exec_command")
    else {
        return batch_result(
            &command,
            tool_err(WorkspaceError::Tool {
                code: "EXEC_ADMISSION_UNAVAILABLE",
                message: "exec_command admission resources are unavailable".into(),
                category: "runtime",
                retryable: true,
            }),
            false,
            false,
            None,
            resource_lock_wait_ms,
            batch_queue_wait_ms,
        );
    };
    let admission_started = Instant::now();
    let workspace_started = Instant::now();
    let permit = match tokio::time::timeout(ADMISSION_TIMEOUT, admission.acquire_owned()).await {
        Ok(Ok(permit)) => permit,
        Ok(Err(error)) => {
            return batch_result(
                &command,
                admission_error(
                    admission_lane,
                    "workspace",
                    admission_limit,
                    workspace_started.elapsed().as_millis(),
                    0,
                    format!("Workspace tool admission lane closed: {error}"),
                ),
                false,
                false,
                None,
                resource_lock_wait_ms,
                batch_queue_wait_ms,
            )
        }
        Err(_) => {
            return batch_result(
                &command,
                admission_error(
                    admission_lane,
                    "workspace",
                    admission_limit,
                    workspace_started.elapsed().as_millis(),
                    0,
                    "Workspace tool admission queue exceeded 30 seconds".into(),
                ),
                false,
                false,
                None,
                resource_lock_wait_ms,
                batch_queue_wait_ms,
            )
        }
    };
    let workspace_admission_wait_ms = workspace_started.elapsed().as_millis();
    let remaining = ADMISSION_TIMEOUT.saturating_sub(admission_started.elapsed());
    let global_started = Instant::now();
    let global_permit =
        match tokio::time::timeout(remaining, global_admission.acquire_owned()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(error)) => {
                return batch_result(
                    &command,
                    admission_error(
                        admission_lane,
                        "global",
                        global_admission_limit,
                        workspace_admission_wait_ms,
                        global_started.elapsed().as_millis(),
                        format!("Global tool admission lane closed: {error}"),
                    ),
                    false,
                    false,
                    None,
                    resource_lock_wait_ms,
                    batch_queue_wait_ms,
                )
            }
            Err(_) => {
                return batch_result(
                    &command,
                    admission_error(
                        admission_lane,
                        "global",
                        global_admission_limit,
                        workspace_admission_wait_ms,
                        global_started.elapsed().as_millis(),
                        "Combined workspace/global admission queue exceeded 30 seconds".into(),
                    ),
                    false,
                    false,
                    None,
                    resource_lock_wait_ms,
                    batch_queue_wait_ms,
                )
            }
        };
    let global_admission_wait_ms = global_started.elapsed().as_millis();
    let admission_queue_wait_ms = admission_started.elapsed().as_millis();
    let mut result = call_exec_tool_async(ctx.as_ref(), "exec_command", &command.args).await;
    if let Some(object) = result.as_object_mut() {
        object.insert("execution_lane".into(), json!("async_process"));
        object.insert("blocking_queue_wait_ms".into(), json!(0));
        attach_admission_metadata(
            object,
            admission_lane,
            admission_limit,
            global_admission_limit,
            workspace_admission_wait_ms,
            global_admission_wait_ms,
            admission_queue_wait_ms,
        );
    }
    let mut result = redaction.redact(result);
    while result.get("process_still_running").and_then(Value::as_bool) == Some(true) {
        let Some(session_id) = result
            .get("session_id")
            .and_then(Value::as_str)
            .map(str::to_string)
        else {
            break;
        };
        let cursor = result
            .get("next_cursor")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        result = match session::wait_command_async(
            &ctx.sessions,
            &json!({
                "session_id": session_id,
                "cursor": cursor,
                "timeout_ms": 120000,
                "until": "finalized",
                "output_mode": "tail"
            }),
        )
        .await
        {
            Ok(value) => value,
            Err(error) => tool_err(error),
        };
    }
    drop(global_permit);
    drop(permit);
    drop(batch_permit);
    let resource_lock_wait_ms = result
        .get("resource_lock_wait_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0) as u128;
    let command_ok = result.get("command_ok").and_then(Value::as_bool) == Some(true);
    batch_result(
        &command,
        result,
        command_ok,
        false,
        None,
        resource_lock_wait_ms,
        batch_queue_wait_ms,
    )
}

fn batch_result(
    command: &ExecBatchCommand,
    result: Value,
    command_ok: bool,
    skipped: bool,
    skip_reason: Option<&str>,
    resource_lock_wait_ms: u128,
    batch_queue_wait_ms: u128,
) -> Value {
    json!({
        "index": command.index,
        "id": command.id,
        "depends_on": command.depends_on,
        "lock_group": command.lock_group,
        "command": command.args,
        "command_ok": command_ok,
        "skipped": skipped,
        "skip_reason": skip_reason,
        "resource_lock_wait_ms": resource_lock_wait_ms,
        "batch_queue_wait_ms": batch_queue_wait_ms,
        "result": result
    })
}

fn skipped_batch_result(command: ExecBatchCommand, reason: &str) -> Value {
    batch_result(
        &command,
        json!({
            "ok": true,
            "command_ok": false,
            "status": "skipped",
            "outcome_class": "skipped",
            "reason": reason
        }),
        false,
        true,
        Some(reason),
        0,
        0,
    )
}

#[allow(clippy::too_many_arguments)]
fn batch_failure_summary(result: &Value) -> Value {
    let nested = result.get("result").unwrap_or(&Value::Null);
    json!({
        "id": result.get("id").cloned().unwrap_or(Value::Null),
        "index": result.get("index").cloned().unwrap_or(Value::Null),
        "status": nested.get("status").cloned().unwrap_or(Value::Null),
        "outcome_class": nested
            .get("outcome_class")
            .cloned()
            .unwrap_or(Value::Null),
        "error_code": nested
            .get("error")
            .and_then(|error| error.get("code"))
            .cloned()
            .unwrap_or(Value::Null),
        "process_exit_code": nested
            .get("process_exit_code")
            .or_else(|| nested.get("exit_code"))
            .cloned()
            .unwrap_or(Value::Null)
    })
}

fn exec_many_output(
    ctx: &ToolContext,
    mode: &str,
    max_parallel: usize,
    stop_on_error: bool,
    commands_requested: usize,
    mut results: Vec<Value>,
    started: Instant,
    warnings: Vec<String>,
    execution_lane: &str,
) -> Value {
    results.sort_by_key(|result| {
        result
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(u64::MAX)
    });
    let explicit_skipped = results
        .iter()
        .filter(|result| result.get("skipped").and_then(Value::as_bool) == Some(true))
        .count();
    let commands_executed = results.len().saturating_sub(explicit_skipped);
    let implicit_skipped = commands_requested.saturating_sub(results.len());
    let skipped_command_count = explicit_skipped + implicit_skipped;
    let failed_command_ids = results
        .iter()
        .filter(|result| {
            result.get("skipped").and_then(Value::as_bool) != Some(true)
                && result.get("command_ok").and_then(Value::as_bool) != Some(true)
        })
        .filter_map(|result| result.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let failed_command_count = failed_command_ids.len();
    let skipped_command_ids = results
        .iter()
        .filter(|result| result.get("skipped").and_then(Value::as_bool) == Some(true))
        .filter_map(|result| result.get("id").and_then(Value::as_str).map(str::to_string))
        .collect::<Vec<_>>();
    let successful_command_count = results
        .iter()
        .filter(|result| result.get("command_ok").and_then(Value::as_bool) == Some(true))
        .count();
    let first_failed_command = results
        .iter()
        .find(|result| {
            result.get("skipped").and_then(Value::as_bool) != Some(true)
                && result.get("command_ok").and_then(Value::as_bool) != Some(true)
        })
        .map(batch_failure_summary);
    let all_commands_ok = failed_command_count == 0
        && skipped_command_count == 0
        && successful_command_count == commands_requested;
    let outcome_class = if all_commands_ok {
        "success"
    } else if successful_command_count > 0 {
        "partial_failure"
    } else {
        "command_failed"
    };
    let (workspace_limit, global_limit) = ctx
        .admission_for("exec_command")
        .map(|(_, workspace, _, global, _)| (workspace, global))
        .unwrap_or((0, 0));
    let batch_summary = if all_commands_ok {
        format!("All {commands_requested} commands succeeded")
    } else {
        format!(
            "{failed_command_count} failed, {skipped_command_count} skipped, {successful_command_count} succeeded"
        )
    };
    let recovery_actions = if failed_command_ids.is_empty() {
        Vec::<Value>::new()
    } else {
        vec![
            json!({
                "action": "inspect_failed_commands",
                "command_ids": failed_command_ids.clone(),
                "reason": "exec_many_command_failure"
            }),
            json!({
                "action": "rerun_failed_commands",
                "tool": "exec_many",
                "command_ids": failed_command_ids,
                "required_arguments": ["commands"],
                "reason": "rerun_only_after_fixing_the_reported_failure"
            }),
        ]
    };
    let mut output = tool_ok(json!({
        "mode": mode,
        "max_parallel": max_parallel,
        "commands_requested": commands_requested,
        "commands_executed": commands_executed,
        "successful_command_count": successful_command_count,
        "failed_command_count": failed_command_count,
        "failed_command_ids": failed_command_ids,
        "skipped_command_count": skipped_command_count,
        "skipped_command_ids": skipped_command_ids,
        "first_failed_command": first_failed_command,
        "batch_summary": batch_summary,
        "stop_on_error": stop_on_error,
        "stopped_early": skipped_command_count > 0 || results.len() < commands_requested,
        "command_ok": all_commands_ok,
        "all_commands_ok": all_commands_ok,
        "outcome_class": outcome_class,
        "recovery_actions": recovery_actions,
        "results": results,
        "duration_ms": started.elapsed().as_millis(),
        "warnings": warnings
    }));
    if let Some(object) = output.as_object_mut() {
        object.insert("execution_lane".into(), json!(execution_lane));
        object.insert("blocking_queue_wait_ms".into(), json!(0));
        object.insert("admission_lane".into(), json!("per_command_process"));
        object.insert("admission_limit".into(), json!(workspace_limit));
        object.insert("global_admission_limit".into(), json!(global_limit));
        object.insert("admission_queue_wait_ms".into(), json!(0));
    }
    output
}
fn attach_admission_metadata(
    object: &mut serde_json::Map<String, Value>,
    lane: &str,
    workspace_limit: usize,
    global_limit: usize,
    workspace_wait_ms: u128,
    global_wait_ms: u128,
    total_wait_ms: u128,
) {
    object.insert("admission_lane".into(), json!(lane));
    object.insert("admission_limit".into(), json!(workspace_limit));
    object.insert("global_admission_limit".into(), json!(global_limit));
    object.insert(
        "workspace_admission_wait_ms".into(),
        json!(workspace_wait_ms),
    );
    object.insert("global_admission_wait_ms".into(), json!(global_wait_ms));
    object.insert("admission_queue_wait_ms".into(), json!(total_wait_ms));
}

fn admission_error(
    lane: &str,
    scope: &str,
    limit: usize,
    workspace_wait_ms: u128,
    global_wait_ms: u128,
    message: String,
) -> Value {
    let queue_wait_ms = workspace_wait_ms.saturating_add(global_wait_ms);
    let mut value = tool_err(WorkspaceError::ToolDetails {
        code: "TOOL_BUSY",
        message,
        category: "runtime",
        retryable: true,
        details: json!({
            "stage": "admission",
            "reason": "concurrency_limit",
            "lane": lane,
            "scope": scope,
            "limit": limit,
            "timeout_ms": ADMISSION_TIMEOUT.as_millis(),
            "suggestion": "稍后重试，或等待当前长任务完成"
        }),
    });
    if let Some(object) = value.as_object_mut() {
        object.insert("execution_lane".into(), json!("admission_control"));
        object.insert("blocking_queue_wait_ms".into(), json!(0));
        object.insert("admission_lane".into(), json!(lane));
        object.insert("admission_limit".into(), json!(limit));
        object.insert("admission_scope".into(), json!(scope));
        object.insert(
            "workspace_admission_wait_ms".into(),
            json!(workspace_wait_ms),
        );
        object.insert("global_admission_wait_ms".into(), json!(global_wait_ms));
        object.insert("admission_queue_wait_ms".into(), json!(queue_wait_ms));
    }
    value
}

const OPERATION_RESULT_BOOLEAN_FIELDS: &[&str] = &[
    "transport_ok",
    "execution_ok",
    "command_ok",
    "verification_ok",
    "process_timed_out",
    "request_timed_out",
    "recoverable",
    "truncated",
    "stdout_truncated",
    "stderr_truncated",
    "cursor_expired",
    "post_checks_pending",
    "detached",
    "deduplicated",
];

const OPERATION_RESULT_TOKEN_FIELDS: &[&str] = &[
    "status",
    "termination_reason",
    "execution_lane",
    "outcome_class",
];

const OPERATION_RESULT_INTEGER_FIELDS: &[&str] = &[
    "exit_code",
    "process_exit_code",
    "elapsed_ms",
    "actual_wait_ms",
    "first_output_ms",
    "stdout_bytes",
    "stderr_bytes",
    "blocking_queue_wait_ms",
    "workspace_admission_wait_ms",
    "global_admission_wait_ms",
    "admission_queue_wait_ms",
    "workspace_lock_wait_ms",
    "operation_lock_wait_ms",
    "resource_lock_wait_ms",
    "history_lock_wait_ms",
    "session_registry_wait_ms",
];

fn operation_summary_token(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).filter(|text| {
        text.len() <= 128
            && text.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')
            })
    })
}

pub(crate) fn operation_result_summary(name: &str, output: &Value) -> Value {
    let mut summary = Map::new();
    summary.insert(
        "ok".into(),
        Value::Bool(output.get("ok").and_then(Value::as_bool) == Some(true)),
    );
    summary.insert("tool".into(), Value::String(name.to_string()));
    summary.insert(
        "affected_files".into(),
        output.get("affected_files").cloned().unwrap_or(Value::Null),
    );
    for field in OPERATION_RESULT_BOOLEAN_FIELDS {
        if let Some(value) = output.get(*field).and_then(Value::as_bool) {
            summary.insert((*field).into(), Value::Bool(value));
        }
    }
    for field in OPERATION_RESULT_TOKEN_FIELDS {
        if let Some(value) = operation_summary_token(output.get(*field)) {
            summary.insert((*field).into(), Value::String(value.to_string()));
        }
    }
    for field in OPERATION_RESULT_INTEGER_FIELDS {
        if let Some(value) = output
            .get(*field)
            .filter(|value| value.is_i64() || value.is_u64())
        {
            summary.insert((*field).into(), value.clone());
        }
    }
    let error = output.get("error").and_then(Value::as_object);
    if let Some(value) = operation_summary_token(
        error
            .and_then(|object| object.get("code"))
            .or_else(|| output.get("error_code")),
    ) {
        summary.insert("error_code".into(), Value::String(value.to_string()));
    }
    if let Some(value) = operation_summary_token(
        error
            .and_then(|object| object.get("category"))
            .or_else(|| output.get("error_category")),
    ) {
        summary.insert("error_category".into(), Value::String(value.to_string()));
    }
    if let Some(value) = error
        .and_then(|object| object.get("retryable"))
        .or_else(|| output.get("retryable"))
        .and_then(Value::as_bool)
    {
        summary.insert("retryable".into(), Value::Bool(value));
    }
    if let Some(count) = output
        .get("warnings")
        .and_then(Value::as_array)
        .map(Vec::len)
    {
        summary.insert("warning_count".into(), json!(count));
    }
    Value::Object(summary)
}

struct TrackedCall {
    task_id: Option<String>,
    operation: Option<OperationRecord>,
}

fn begin_tracked_call(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    effective_args: &Value,
) -> Result<TrackedCall, Value> {
    let task_id = if requires_write_baseline(name, effective_args) {
        let task = ctx.harness.current_task().ok().flatten();
        if let Some(task) = task {
            if let Err(error) = ctx.harness.check_baseline(&task.id) {
                return Err(attach_harness_status(
                    ctx,
                    tool_err_code(error.code(), error.to_string(), "permission"),
                    false,
                ));
            }
            let _ = ctx.harness.record_event(
                &task.id,
                "operation_started",
                Some(name),
                operation_input(args),
                json!({"ok": true, "tracking": "task"}),
            );
            Some(task.id)
        } else {
            None
        }
    } else {
        None
    };

    let operation = if should_log_operation(name) {
        ctx.harness
            .record_operation(
                None,
                task_id.as_deref(),
                name,
                "started",
                json!({"arguments_present": !args.is_null()}),
                json!({"ok": true}),
            )
            .ok()
    } else {
        None
    };

    Ok(TrackedCall { task_id, operation })
}

fn finish_tracked_call(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    tracking: TrackedCall,
    mut output: Value,
) -> Value {
    if tracking.task_id.is_none()
        && standalone_operation(name)
        && output.get("ok") == Some(&Value::Bool(true))
    {
        attach_standalone_metadata(
            &mut output,
            "当前操作已在 standalone 模式完成；如需继续，直接调用下一个开发工具。",
        );
    }
    if let Some(operation) = tracking.operation.as_ref() {
        if let Some(object) = output.as_object_mut() {
            let field = if object.contains_key("operation_id") {
                "harness_operation_id"
            } else {
                "operation_id"
            };
            object.insert(field.into(), Value::String(operation.id.clone()));
        }
    }
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        output = attach_harness_status(ctx, output, tracking.task_id.is_none());
    }
    let deferred_process_operation = tracking.operation.as_ref().is_some_and(|operation| {
        if output.get("command_ok") != Some(&Value::Null) {
            return false;
        }
        let Some(session_id) = output.get("session_id").and_then(Value::as_str) else {
            return false;
        };
        let Ok(session) = ctx.sessions.get(session_id) else {
            return false;
        };
        let input_summary = operation_input(args);
        let mut deferred = operation.clone();
        deferred.reason = input_summary
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        deferred.input_summary = input_summary;
        session.attach_harness_operation(ctx.harness.clone(), deferred);
        true
    });
    if let Some(task_id) = tracking.task_id.as_deref() {
        let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
        let _ = ctx.harness.record_event(
            task_id,
            "operation_finished",
            Some(name),
            operation_input(args),
            json!({"ok": succeeded, "tool": name}),
        );
        if succeeded {
            let _ = ctx.harness.refresh_expected_state(task_id);
        }
    }
    if let Some(operation) = tracking.operation.filter(|_| !deferred_process_operation) {
        let succeeded = output.get("ok").and_then(Value::as_bool) == Some(true);
        let _ = ctx.harness.record_operation(
            Some(&operation.id),
            tracking.task_id.as_deref(),
            name,
            if succeeded { "completed" } else { "failed" },
            operation_input(args),
            operation_result_summary(name, &output),
        );
    }
    output
}

async fn call_exec_tool_async(ctx: &ToolContext, name: &str, args: &Value) -> Value {
    call_exec_tool_async_with_policy(ctx, name, args, false).await
}

async fn call_exec_tool_async_with_policy(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    permission_override: bool,
) -> Value {
    let effective_args = apply_default_cwd(ctx, name, args);
    let mut override_policy;
    let policy = if permission_override {
        override_policy = ctx.policy.clone();
        override_policy.permission_mode = "dangerous".into();
        &override_policy
    } else {
        &ctx.policy
    };
    if let Err(error) =
        validate_tool_arguments_for_workspace(name, &effective_args, policy, Some(&ctx.workspace))
    {
        return policy_tool_err(ctx, name, &effective_args, error);
    }

    let tracking = match begin_tracked_call(ctx, name, args, &effective_args) {
        Ok(tracking) => tracking,
        Err(output) => return output,
    };
    let output = match exec::exec_command_async(ctx, &effective_args).await {
        Ok(value) => value,
        Err(error) => tool_err(error),
    };
    finish_tracked_call(ctx, name, args, tracking, output)
}

async fn call_permission_tool_async(ctx: SharedToolContext, name: &str, args: &Value) -> Value {
    let effective_args = apply_default_cwd(ctx.as_ref(), name, args);
    if let Err(error) = validate_tool_arguments_for_workspace(
        name,
        &effective_args,
        &ctx.policy,
        Some(&ctx.workspace),
    ) {
        return policy_tool_err(ctx.as_ref(), name, &effective_args, error);
    }

    let tracking = match begin_tracked_call(ctx.as_ref(), name, args, &effective_args) {
        Ok(tracking) => tracking,
        Err(output) => return output,
    };
    let result = if effective_args.get("resume_id").is_some() {
        resume_pending_operation_async(ctx.clone(), &effective_args).await
    } else {
        request_permissions(ctx.as_ref(), &effective_args)
    };
    let output = match result {
        Ok(value) => value,
        Err(error) => tool_err(error),
    };
    finish_tracked_call(ctx.as_ref(), name, args, tracking, output)
}

async fn resume_pending_operation_async(
    ctx: SharedToolContext,
    args: &Value,
) -> Result<Value, WorkspaceError> {
    let resume_id = args
        .get("resume_id")
        .and_then(Value::as_str)
        .ok_or_else(|| WorkspaceError::invalid_argument("resume_id is required"))?;
    let operation = ctx.pending_operations.take(resume_id)?;
    let explicitly_approved = args
        .get("approve")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && args
            .get("confirm")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let approved = explicitly_approved || ctx.policy.skip_permission_gates();
    if !approved {
        ctx.pending_operations.put_back(operation);
        return Err(WorkspaceError::ToolDetails {
            code: "PERMISSION_NOT_APPROVED",
            message: "The pending operation was not approved.".into(),
            category: "permission",
            retryable: true,
            details: json!({
                "resume_id": resume_id,
                "suggestion": "取得用户授权后以 approve=true、confirm=true 重试 request_permissions"
            }),
        });
    }

    let mut resumed_args = operation.arguments.clone();
    if matches!(
        operation.permission.as_str(),
        "destructive_command" | "shell_expansion" | "inline_script"
    ) {
        if let Some(object) = resumed_args.as_object_mut() {
            object.insert("confirm".into(), Value::Bool(true));
        }
    }

    let resumed_execution_lane;
    let mut resumed = if operation.tool_name == "exec_command" {
        resumed_execution_lane = "async_process";
        call_exec_tool_async_with_policy(ctx.as_ref(), &operation.tool_name, &resumed_args, true)
            .await
    } else {
        resumed_execution_lane = "blocking_worker";
        let retry_operation = operation.clone();
        let worker_ctx = ctx.clone();
        let tool_name = operation.tool_name.clone();
        let worker_args = resumed_args.clone();
        match tokio::task::spawn_blocking(move || {
            call_tool_inner(worker_ctx.as_ref(), &tool_name, &worker_args, true)
        })
        .await
        {
            Ok(value) => value,
            Err(error) => {
                ctx.pending_operations.put_back(retry_operation);
                return Err(WorkspaceError::ToolDetails {
                    code: "TOOL_WORKER_FAILED",
                    message: format!("Permission resume worker failed: {error}"),
                    category: "runtime",
                    retryable: true,
                    details: json!({
                        "stage": "permission_resume",
                        "reason": "join_failed",
                        "resume_id": resume_id,
                        "suggestion": "使用同一 resume_id 重试 request_permissions"
                    }),
                });
            }
        }
    };

    if let Some(object) = resumed.as_object_mut() {
        object.insert("resumed".into(), Value::Bool(true));
        object.insert("resume_id".into(), Value::String(operation.resume_id));
        object.insert(
            "resumed_execution_lane".into(),
            Value::String(resumed_execution_lane.into()),
        );
        object.insert(
            "permission_grant".into(),
            json!({
                "status": "granted_and_resumed",
                "permission": operation.permission,
                "reason": operation.reason,
                "scope": args.get("scope").and_then(Value::as_str).unwrap_or("once")
            }),
        );
    }
    Ok(resumed)
}

async fn call_session_tool_async(ctx: &ToolContext, name: &str, args: &Value) -> Value {
    let effective_args = apply_default_cwd(ctx, name, args);
    if let Err(error) = validate_tool_arguments_for_workspace(
        name,
        &effective_args,
        &ctx.policy,
        Some(&ctx.workspace),
    ) {
        return policy_tool_err(ctx, name, &effective_args, error);
    }

    let result = match name {
        "wait_command" => session::wait_command_async(&ctx.sessions, &effective_args).await,
        "resolve_operation" => {
            session::resolve_operation_async(&ctx.sessions, &effective_args).await
        }
        "list_sessions" => session::list_sessions(&ctx.sessions, &effective_args),
        "send_input" => session::send_input_async(&ctx.sessions, &effective_args).await,
        "read_output" => session::read_output_async(&ctx.sessions, &effective_args).await,
        "kill_session" => session::kill_session_async(&ctx.sessions, &effective_args).await,
        _ => unreachable!("non-session tool routed to async session dispatcher"),
    };
    let mut output = match result {
        Ok(value) => value,
        Err(error) => tool_err(error),
    };
    if let Some(object) = output.as_object_mut() {
        object.insert("execution_lane".into(), json!("async_control"));
        object.insert("blocking_queue_wait_ms".into(), json!(0));
        object.insert("admission_lane".into(), json!("async_control"));
        object.insert("admission_limit".into(), json!(0));
        object.insert("admission_queue_wait_ms".into(), json!(0));
    }
    if output.get("ok").and_then(Value::as_bool) == Some(false) {
        output = attach_harness_status(ctx, output, true);
    }
    output
}

fn call_tool_inner(
    ctx: &ToolContext,
    name: &str,
    args: &Value,
    permission_override: bool,
) -> Value {
    let effective_args = apply_default_cwd(ctx, name, args);
    let mut override_policy;
    let policy = if permission_override {
        override_policy = ctx.policy.clone();
        override_policy.permission_mode = "dangerous".into();
        &override_policy
    } else {
        &ctx.policy
    };
    if let Err(e) =
        validate_tool_arguments_for_workspace(name, &effective_args, policy, Some(&ctx.workspace))
    {
        return policy_tool_err(ctx, name, &effective_args, e);
    }

    if crate::harness::tools::TOOL_NAMES.contains(&name) {
        return match crate::harness::tools::call(ctx, name, args) {
            Ok(value) => value,
            Err(error) => attach_harness_status(ctx, tool_err(error), false),
        };
    }

    let tracking = match begin_tracked_call(ctx, name, args, &effective_args) {
        Ok(tracking) => tracking,
        Err(output) => return output,
    };

    let ws = &ctx.workspace;
    let result = match name {
        "history_session_bootstrap" => history::bootstrap(ctx, &effective_args),
        "history_session_checkpoint" => history::checkpoint(ctx, &effective_args),
        "history_session_validate" => history::validate(ctx, &effective_args),
        "server_info" => server_info(ctx),
        "query_tool_usage" => crate::tools::tool_usage::query_tool_usage(ctx, &effective_args),
        "exec_health_check" => exec::exec_health_check(ctx),
        "set_default_cwd" => set_default_cwd(ctx, &effective_args),
        "read_file" => file::read_file(ws, &effective_args),
        "read_many" => file::read_many(ws, &effective_args),
        "project_map" => project::project_map(ws, &effective_args),
        "list_files" => file::list_files(ws, &effective_args),
        "search_text" => file::search_text(ws, &effective_args),
        "patch_check" => patch::patch_check(ctx, &effective_args),
        "apply_patch" => patch::apply_patch(ctx, &effective_args),
        "edit" => patch::edit(ctx, &effective_args),
        "edit_file" => patch::edit_file(ctx, &effective_args),
        "edit_many" => patch::edit_many(ctx, &effective_args),
        "file_ops" => patch::file_ops(ctx, &effective_args),
        "format_files" => file_action::format_files(ctx, &effective_args),
        "exec_command" => exec::exec_command(ctx, &effective_args),
        "exec_many" => Ok(call_exec_many_sync(ctx, &effective_args)),
        "wait_command" => session::wait_command(&ctx.sessions, &effective_args),
        "resolve_operation" => session::resolve_operation(&ctx.sessions, &effective_args),
        "list_sessions" => session::list_sessions(&ctx.sessions, &effective_args),
        "send_input" => session::send_input(&ctx.sessions, &effective_args),
        "read_output" => session::read_output(&ctx.sessions, &effective_args),
        "kill_session" => session::kill_session(&ctx.sessions, &effective_args),
        "git_status" => git::git_status(ws, &effective_args),
        "git_diff" => git::git_diff(ws, &effective_args),
        "git_log" => git::git_log(ws, &effective_args),
        "git_show" => git::git_show(ws, &effective_args),
        "git_blame" => git::git_blame(ws, &effective_args),
        "git_branch" => git::git_branch(ws, &effective_args),
        "git_stage" => git::git_stage(ws, &effective_args),
        "git_commit" => git::git_commit(ws, &effective_args),
        "git_restore" => git::git_restore(ws, &effective_args),
        "view_image" => image_tool::view_image(ws, &effective_args),
        "request_permissions" => request_permissions(ctx, &effective_args),
        _ => {
            return tool_err_code(
                "INVALID_ARGUMENT",
                format!("Unknown tool: {name}"),
                "validation",
            )
        }
    };
    let output = match result {
        Ok(v) => v,
        Err(e) => tool_err(e),
    };
    finish_tracked_call(ctx, name, args, tracking, output)
}

fn request_permissions(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    if let Some(resume_id) = args.get("resume_id").and_then(Value::as_str) {
        let operation = ctx.pending_operations.take(resume_id)?;
        let explicitly_approved = args
            .get("approve")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            && args
                .get("confirm")
                .and_then(Value::as_bool)
                .unwrap_or(false);
        let approved = explicitly_approved || ctx.policy.skip_permission_gates();
        if !approved {
            ctx.pending_operations.put_back(operation);
            return Err(WorkspaceError::ToolDetails {
                code: "PERMISSION_NOT_APPROVED",
                message: "The pending operation was not approved.".into(),
                category: "permission",
                retryable: true,
                details: json!({
                    "resume_id": resume_id,
                    "suggestion": "取得用户授权后以 approve=true、confirm=true 重试 request_permissions"
                }),
            });
        }

        let mut resumed_args = operation.arguments.clone();
        if matches!(
            operation.permission.as_str(),
            "destructive_command" | "shell_expansion" | "inline_script"
        ) {
            if let Some(object) = resumed_args.as_object_mut() {
                object.insert("confirm".into(), Value::Bool(true));
            }
        }
        let mut resumed = call_tool_inner(ctx, &operation.tool_name, &resumed_args, true);
        if let Some(object) = resumed.as_object_mut() {
            object.insert("resumed".into(), Value::Bool(true));
            object.insert("resume_id".into(), Value::String(operation.resume_id));
            object.insert(
                "permission_grant".into(),
                json!({
                    "status": "granted_and_resumed",
                    "permission": operation.permission,
                    "reason": operation.reason,
                    "scope": args.get("scope").and_then(Value::as_str).unwrap_or("once")
                }),
            );
        }
        return Ok(resumed);
    }

    if ctx.policy.skip_permission_gates() {
        Ok(tool_ok(json!({
            "ok": true,
            "status": "granted",
            "grant_id": "dangerously-skip-all-permissions",
            "expires_at": null,
            "constraints": {
                "mode": "dangerous",
                "workspace": ctx.workspace.root_display(),
                "requested": args
            },
            "warnings": [
                "dangerous permission mode is enabled; permission-gated operations are auto-granted"
            ]
        })))
    } else {
        Ok(tool_ok(json!({
            "ok": false,
            "status": "unsupported",
            "grant_id": null,
            "expires_at": null,
            "next_actions": [],
            "error": {
                "code": "RESUME_ID_REQUIRED",
                "message": "Provide the resume_id returned by the blocked operation.",
                "category": "permission",
                "retryable": true,
                "details": { "requested": args }
            }
        })))
    }
}

fn apply_default_cwd<'a>(ctx: &ToolContext, name: &str, args: &'a Value) -> Cow<'a, Value> {
    let base = if ctx.default_cwd_path() == ctx.workspace.root() {
        ".".to_string()
    } else {
        ctx.default_cwd_display()
    };
    if base == "." {
        return Cow::Borrowed(args);
    }

    let mut effective = args.clone();
    match name {
        "exec_command" if effective.get("workdir").is_none() && effective.get("cwd").is_none() => {
            effective["workdir"] = Value::String(base.clone());
        }
        "list_files" | "project_map" | "git_status" | "git_log" => {
            let path = effective.get("path").and_then(Value::as_str).unwrap_or(".");
            effective["path"] = Value::String(prefix_relative_path(&base, path));
        }
        "read_file" | "search_text" | "git_blame" | "view_image" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
        }
        "read_many" => {
            if let Some(items) = effective.get("items").and_then(Value::as_array).cloned() {
                effective["items"] = Value::Array(
                    items
                        .into_iter()
                        .map(|mut item| {
                            if let Some(path) = item.get("path").and_then(Value::as_str) {
                                item["path"] = Value::String(prefix_relative_path(&base, path));
                            }
                            item
                        })
                        .collect(),
                );
            }
        }
        "git_diff" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
            if let Some(paths) = effective.get("paths").and_then(Value::as_array).cloned() {
                effective["paths"] = Value::Array(
                    paths
                        .iter()
                        .map(|path| {
                            path.as_str()
                                .map(|value| Value::String(prefix_relative_path(&base, value)))
                                .unwrap_or_else(|| path.clone())
                        })
                        .collect(),
                );
            }
        }
        "format_files" => {
            if let Some(paths) = effective.get("paths").and_then(Value::as_array).cloned() {
                effective["paths"] = Value::Array(
                    paths
                        .iter()
                        .map(|path| {
                            path.as_str()
                                .map(|value| Value::String(prefix_relative_path(&base, value)))
                                .unwrap_or_else(|| path.clone())
                        })
                        .collect(),
                );
            } else if matches!(
                effective.get("scope").and_then(Value::as_str),
                Some("changed" | "staged" | "project")
            ) {
                effective["paths"] = json!([base.clone()]);
            }
            if let Some(hashes) = effective
                .get("expected_sha256")
                .and_then(Value::as_object)
                .cloned()
            {
                effective["expected_sha256"] = Value::Object(
                    hashes
                        .into_iter()
                        .map(|(path, hash)| (prefix_relative_path(&base, &path), hash))
                        .collect(),
                );
            }
        }
        "apply_patch" | "patch_check" => {
            if let Some(patch) = effective.get("patch").and_then(Value::as_str) {
                effective["patch"] = Value::String(prefix_patch_paths(&base, patch));
            }
            if let Some(hashes) = effective
                .get("expected_sha256")
                .and_then(Value::as_object)
                .cloned()
            {
                effective["expected_sha256"] = Value::Object(
                    hashes
                        .into_iter()
                        .map(|(path, hash)| (prefix_relative_path(&base, &path), hash))
                        .collect(),
                );
            }
        }
        "edit_file" => {
            if let Some(path) = effective.get("path").and_then(Value::as_str) {
                effective["path"] = Value::String(prefix_relative_path(&base, path));
            }
        }
        "edit" | "edit_many" => {
            prefix_array_paths(&mut effective, "files", &base, &["path"]);
        }
        "file_ops" => {
            prefix_array_paths(
                &mut effective,
                "operations",
                &base,
                &["path", "destination"],
            );
        }
        "git_stage" | "git_commit" | "git_restore" => {
            if let Some(paths) = effective.get("paths").and_then(Value::as_array).cloned() {
                effective["paths"] = Value::Array(
                    paths
                        .into_iter()
                        .map(|path| {
                            path.as_str()
                                .map(|value| Value::String(prefix_relative_path(&base, value)))
                                .unwrap_or(path)
                        })
                        .collect(),
                );
            }
        }
        _ => {}
    }
    Cow::Owned(effective)
}

fn prefix_array_paths(value: &mut Value, array_key: &str, base: &str, keys: &[&str]) {
    if let Some(items) = value.get(array_key).and_then(Value::as_array).cloned() {
        value[array_key] = Value::Array(
            items
                .into_iter()
                .map(|mut item| {
                    for key in keys {
                        if let Some(path) = item.get(*key).and_then(Value::as_str) {
                            item[*key] = Value::String(prefix_relative_path(base, path));
                        }
                    }
                    item
                })
                .collect(),
        );
    }
}

fn prefix_relative_path(base: &str, path: &str) -> String {
    if path == "." || path.is_empty() {
        return base.to_string();
    }
    if Path::new(path).is_absolute() || path.starts_with("..") {
        return path.to_string();
    }
    format!("{base}/{}", path.trim_start_matches("./"))
}

fn prefix_patch_paths(base: &str, patch: &str) -> String {
    patch
        .lines()
        .map(|line| {
            for marker in ["--- a/", "+++ b/"] {
                if let Some(path) = line.strip_prefix(marker) {
                    return format!("{marker}{base}/{path}");
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn requires_write_baseline(name: &str, args: &Value) -> bool {
    match name {
        "exec_command" => true,
        "apply_patch" => !args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "edit" | "edit_file" | "edit_many" | "file_ops" => !args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        "format_files" => args.get("mode").and_then(Value::as_str) == Some("apply"),
        "git_branch" | "git_stage" | "git_commit" | "git_restore" => !args
            .get("dry_run")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        _ => false,
    }
}

fn standalone_operation(name: &str) -> bool {
    matches!(
        name,
        "patch_check"
            | "apply_patch"
            | "edit"
            | "edit_file"
            | "edit_many"
            | "file_ops"
            | "format_files"
            | "exec_command"
            | "git_branch"
            | "git_stage"
            | "git_commit"
            | "git_restore"
    )
}

fn should_log_operation(name: &str) -> bool {
    standalone_operation(name)
        || matches!(
            name,
            "git_status"
                | "git_diff"
                | "git_log"
                | "git_show"
                | "git_blame"
                | "git_branch"
                | "git_stage"
                | "git_commit"
                | "git_restore"
        )
}

fn operation_input(args: &Value) -> Value {
    json!({
        "arguments_present": !args.is_null(),
        "reason": args.get("reason")
    })
}

fn attach_harness_status(ctx: &ToolContext, mut output: Value, standalone: bool) -> Value {
    if let Ok(mut status) = ctx.harness.status() {
        if standalone && status.task_id.is_none() {
            status.next_actions.clear();
        }
        status.next_actions = filter_exposed_actions(ctx, status.next_actions);
        if let Some(object) = output.as_object_mut() {
            object.insert(
                "harness".into(),
                serde_json::to_value(status).unwrap_or_else(|_| {
                    json!({
                        "status": "unavailable",
                        "reason": "无法序列化 Harness 状态"
                    })
                }),
            );
            if standalone {
                attach_standalone_metadata(
                    &mut output,
                    "命令未成功；请检查 stderr、exit_code 或调整参数后重试。",
                );
            }
        }
    }
    output
}

fn attach_standalone_metadata(output: &mut Value, recovery_hint: &str) {
    if let Some(object) = output.as_object_mut() {
        object.insert("harness_mode".into(), Value::String("standalone".into()));
        object.insert("task_required".into(), Value::Bool(false));
        object.entry("next_actions").or_insert_with(|| json!([]));
        object.insert(
            "recovery_hint".into(),
            Value::String(recovery_hint.to_string()),
        );
    }
}

fn filter_exposed_actions(ctx: &ToolContext, actions: Vec<String>) -> Vec<String> {
    let exposed = crate::tools::registry::exposed_tool_names(&ctx.tool_profile);
    actions
        .into_iter()
        .filter(|action| exposed.contains(&action.as_str()))
        .collect()
}

pub fn server_info(ctx: &ToolContext) -> Result<Value, WorkspaceError> {
    let tools = crate::tools::registry::exposed_tool_names(&ctx.tool_profile);
    let (blocking_limit, global_blocking_limit) = ctx
        .admission_for("read_file")
        .map(|(_, local, _, global, _)| (local, global))
        .unwrap_or((0, 0));
    let (process_limit, global_process_limit) = ctx
        .admission_for("exec_command")
        .map(|(_, local, _, global, _)| (local, global))
        .unwrap_or((0, 0));
    Ok(tool_ok(json!({
        "server": "coding-tools-mcp",
        "title": "Coding Tools MCP",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol_version": crate::mcp::LATEST_PROTOCOL_VERSION,
        "supported_protocol_versions": crate::mcp::SUPPORTED_PROTOCOL_VERSIONS,
        "workspace": ctx.workspace.root_display(),
        "permission_mode": ctx.permission_mode,
        "default_cwd": ctx.default_cwd_display(),
        "network_allowed": ctx.policy.network_allowed(),
        "tool_profile": ctx.tool_profile,
        "toolset_revision": crate::tools::registry::toolset_revision(&ctx.tool_profile),
        "auth_enabled": ctx.auth.auth_enabled(),
        "auth_type": ctx.auth.auth_type,
        "endpoint_path": "/mcp",
        "concurrency": {
            "fast_lane": "inline",
            "shared_across_transports": true,
            "workspace_blocking_admission_limit": blocking_limit,
            "workspace_process_admission_limit": process_limit,
            "global_blocking_admission_limit": global_blocking_limit,
            "global_process_admission_limit": global_process_limit,
            "admission_scope": "global_plus_workspace",
            "active_session_limit": ctx.sessions.active_session_limit(),
            "active_session_slots_available": ctx.sessions.active_slots_available(),
            "admission_timeout_ms": ADMISSION_TIMEOUT.as_millis(),
            "session_admission_timeout_ms": 1000
        },
        "environment": {
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "current_executable": std::env::current_exe().ok().map(|path| path.display().to_string()),
            "powershell": exec::powershell_environment(),
            "filesystem_sandbox": {
                "available": false,
                "enforced": false,
                "default_scope": "workspace",
                "host_scope_available": false
            },
            "workspace_exec": {
                "available": true,
                "sandbox_enforced": false,
                "boundary": "policy_only",
                "workspace_local_entries": ctx.policy.workspace_local_entries,
                "script_extensions": ctx.policy.workspace_script_extensions.iter().cloned().collect::<Vec<_>>(),
                "system_command_allowlist": ctx.policy.allowed_commands.iter().cloned().collect::<Vec<_>>()
            }
        },
        "tools": tools,
        "tool_count": tools.len()
    })))
}

pub fn set_default_cwd(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ctx.workspace.resolve_existing(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory(
            "Default cwd must be a directory",
        ));
    }
    ctx.set_default_cwd(resolved.path.clone());
    Ok(tool_ok(json!({
        "workspace": ctx.workspace.root_display(),
        "default_cwd": resolved.display,
        "resolved_cwd": resolved.path.display().to_string()
    })))
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use serde_json::json;

    use crate::tools::parallel_stats::ParallelPairStats;
    use crate::tools::ToolContext;

    use super::{
        apply_default_cwd, begin_tracked_call, call_tool_async, collect_parallelism_observations,
        command_parallel_signature, default_exec_many_parallelism, finish_tracked_call,
        parallel_pair_key, parse_exec_batch_commands, resolve_exec_many_decision_with_history,
    };

    #[test]
    fn default_cwd_rewrite_borrows_until_a_path_change_is_required() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context");
        let arguments = json!({
            "patch": "x".repeat(256 * 1024),
            "confirm": true
        });

        let unchanged = apply_default_cwd(&ctx, "apply_patch", &arguments);
        assert!(matches!(unchanged, Cow::Borrowed(_)));

        let subdir = workspace.path().join("subdir");
        std::fs::create_dir(&subdir).expect("create subdir");
        ctx.set_default_cwd(subdir);
        let rewritten = apply_default_cwd(&ctx, "apply_patch", &arguments);
        assert!(matches!(rewritten, Cow::Owned(_)));
    }

    #[test]
    fn harness_tracking_preserves_execution_operation_id() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx =
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context");
        let args = json!({"program": "cargo", "args": ["test"]});
        let tracking =
            begin_tracked_call(&ctx, "exec_command", &args, &args).expect("tracked call");

        let result = finish_tracked_call(
            &ctx,
            "exec_command",
            &args,
            tracking,
            json!({
                "ok": true,
                "operation_id": "auto:execution-operation",
                "command_ok": false,
                "verification_ok": false,
                "termination_reason": "exited",
                "process_exit_code": 7,
                "warnings": ["bounded warning"],
                "command": "must-not-persist",
                "stdout": "must-not-persist"
            }),
        );

        assert_eq!(
            result["operation_id"], "auto:execution-operation",
            "{result}"
        );
        assert!(result["harness_operation_id"].is_string(), "{result}");
        assert_ne!(
            result["harness_operation_id"], result["operation_id"],
            "{result}"
        );
        let operations = ctx.harness.list_operations(0, 10).expect("operation log");
        let terminal = operations
            .iter()
            .find(|operation| operation.kind == "completed")
            .expect("completed operation");
        assert_eq!(terminal.result_summary["command_ok"], false);
        assert_eq!(terminal.result_summary["verification_ok"], false);
        assert_eq!(terminal.result_summary["termination_reason"], "exited");
        assert_eq!(terminal.result_summary["process_exit_code"], 7);
        assert_eq!(terminal.result_summary["warning_count"], 1);
        assert!(terminal.result_summary.get("command").is_none());
        assert!(terminal.result_summary.get("stdout").is_none());
    }

    #[test]
    fn exec_many_auto_scheduler_combines_hard_rules_and_history() {
        let independent = parse_exec_batch_commands(&[
            json!({"program": "python", "args": ["--version"]}),
            json!({"program": "node", "args": ["--version"]}),
        ])
        .expect("independent commands");
        let decision =
            resolve_exec_many_decision_with_history("auto", &independent, 8, &BTreeMap::new());
        assert_eq!(decision.mode, "parallel");
        assert_eq!(decision.source, "hard_rules");

        let dag = parse_exec_batch_commands(&[
            json!({"id": "prepare", "program": "python", "args": ["--version"]}),
            json!({"id": "finish", "depends_on": ["prepare"], "program": "node", "args": ["--version"]}),
        ])
        .expect("dag commands");
        let decision = resolve_exec_many_decision_with_history("auto", &dag, 8, &BTreeMap::new());
        assert_eq!(decision.mode, "dag");
        assert_eq!(decision.source, "dependency_graph");

        let opaque = parse_exec_batch_commands(&[
            json!({"cmd": "echo first"}),
            json!({"cmd": "echo second"}),
        ])
        .expect("opaque commands");
        let decision =
            resolve_exec_many_decision_with_history("auto", &opaque, 8, &BTreeMap::new());
        assert_eq!(decision.mode, "sequential");
        assert_eq!(decision.source, "hard_safety_rule");

        let evidence_required = parse_exec_batch_commands(&[
            json!({"program": "python", "args": ["first.py"], "workdir": "a"}),
            json!({"program": "node", "args": ["second.js"], "workdir": "b"}),
        ])
        .expect("evidence-required commands");
        let decision = resolve_exec_many_decision_with_history(
            "auto",
            &evidence_required,
            8,
            &BTreeMap::new(),
        );
        assert_eq!(decision.mode, "sequential");
        assert_eq!(decision.source, "insufficient_history");

        let pair = parallel_pair_key(
            &evidence_required[0].parallel_signature,
            &evidence_required[1].parallel_signature,
        );
        let mut safe_history = BTreeMap::new();
        safe_history.insert(
            pair.clone(),
            ParallelPairStats {
                attempts: 5,
                successes: 5,
                ..Default::default()
            },
        );
        let decision =
            resolve_exec_many_decision_with_history("auto", &evidence_required, 8, &safe_history);
        assert_eq!(decision.mode, "parallel");
        assert_eq!(decision.source, "historical_statistics");
        assert_eq!(decision.history_samples, 5);

        let mut conflict_history = BTreeMap::new();
        conflict_history.insert(
            pair,
            ParallelPairStats {
                attempts: 5,
                successes: 3,
                conflicts: 2,
                ..Default::default()
            },
        );
        let decision = resolve_exec_many_decision_with_history(
            "auto",
            &evidence_required,
            8,
            &conflict_history,
        );
        assert_eq!(decision.mode, "sequential");
        assert_eq!(decision.source, "historical_conflict");

        let locked = parse_exec_batch_commands(&[
            json!({"program": "cargo", "args": ["test"], "workdir": "crate-a"}),
            json!({"program": "git", "args": ["commit", "-m", "test"], "workdir": "."}),
            json!({"program": "npm", "args": ["install"], "workdir": "web"}),
        ])
        .expect("locked commands");
        assert_eq!(
            locked[0].lock_group.as_deref(),
            Some("cargo-target:crate-a")
        );
        assert_eq!(locked[1].lock_group.as_deref(), Some("git-index:."));
        assert_eq!(locked[2].lock_group.as_deref(), Some("node-generated:web"));
        assert!(locked.iter().all(|command| command.lock_group_inferred));

        let private_signature = command_parallel_signature(&json!({
            "program": "C:\\private\\CustomerDeployTool.exe",
            "args": ["run"],
            "workdir": "customer-secret-workspace"
        }));
        assert!(private_signature.starts_with("custom-"));
        assert!(!private_signature.contains("customerdeploytool"));
        assert!(!private_signature.contains("customer-secret-workspace"));

        assert_eq!(default_exec_many_parallelism(20, 64), 8);
        assert_eq!(default_exec_many_parallelism(20, 4), 4);
        assert_eq!(default_exec_many_parallelism(1, 64), 1);
    }

    #[test]
    fn exec_many_parallel_observations_classify_overlap_conflict_and_serialization() {
        let commands = parse_exec_batch_commands(&[
            json!({"program": "python", "args": ["--version"]}),
            json!({"program": "node", "args": ["--version"]}),
        ])
        .expect("commands");
        let results = vec![
            json!({
                "id": "command-0",
                "command_ok": true,
                "resource_lock_wait_ms": 0,
                "result": {"started_ts_ms": 1000, "elapsed_ms": 500}
            }),
            json!({
                "id": "command-1",
                "command_ok": true,
                "resource_lock_wait_ms": 0,
                "result": {"started_ts_ms": 1200, "elapsed_ms": 500}
            }),
        ];
        let (observations, truncated) =
            collect_parallelism_observations(&commands, &results, "parallel");
        assert!(!truncated);
        assert_eq!(observations.len(), 1);
        assert_eq!(observations[0]["outcome"], "success");
        assert_eq!(observations[0]["overlap_ms"], 300);

        let conflict_results = vec![
            json!({
                "id": "command-0",
                "command_ok": false,
                "resource_lock_wait_ms": 0,
                "result": {
                    "started_ts_ms": 1000,
                    "elapsed_ms": 500,
                    "stderr": "fatal: Unable to create '.git/index.lock': File exists"
                }
            }),
            json!({
                "id": "command-1",
                "command_ok": true,
                "resource_lock_wait_ms": 0,
                "result": {"started_ts_ms": 1200, "elapsed_ms": 500}
            }),
        ];
        let (observations, _) =
            collect_parallelism_observations(&commands, &conflict_results, "parallel");
        assert_eq!(observations[0]["outcome"], "conflict");

        let locked_commands = parse_exec_batch_commands(&[
            json!({"program": "cargo", "args": ["test"], "workdir": "."}),
            json!({"program": "cargo", "args": ["check"], "workdir": "."}),
        ])
        .expect("locked commands");
        let serialized_results = vec![
            json!({
                "id": "command-0",
                "command_ok": true,
                "resource_lock_wait_ms": 0,
                "result": {"started_ts_ms": 1000, "elapsed_ms": 200}
            }),
            json!({
                "id": "command-1",
                "command_ok": true,
                "resource_lock_wait_ms": 250,
                "result": {"started_ts_ms": 1300, "elapsed_ms": 200}
            }),
        ];
        let (observations, _) =
            collect_parallelism_observations(&locked_commands, &serialized_results, "parallel");
        assert_eq!(observations[0]["outcome"], "serialized");
    }

    #[tokio::test]
    async fn exec_many_runs_sequentially_and_stops_after_failure() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let result = call_tool_async(
            ctx,
            "exec_many".into(),
            json!({
                "mode": "sequential",
                "commands": [
                    { "program": "cargo", "args": ["--version"] },
                    { "program": "coding-tools-command-that-does-not-exist" },
                    { "program": "cargo", "args": ["--version"] }
                ],
                "stop_on_error": true
            }),
        )
        .await;

        assert_eq!(result["commands_requested"], 3);
        assert_eq!(result["commands_executed"], 2);
        assert_eq!(result["failed_command_count"], 1);
        assert_eq!(result["failed_command_ids"], json!(["command-1"]));
        assert_eq!(result["skipped_command_ids"], json!([]));
        assert_eq!(result["first_failed_command"]["id"], "command-1");
        assert!(result["batch_summary"]
            .as_str()
            .expect("batch summary")
            .contains("1 failed"));
        assert_eq!(result["recovery_actions"].as_array().unwrap().len(), 2);
        assert_eq!(result["skipped_command_count"], 1);
        assert_eq!(result["stopped_early"], true);
        assert_eq!(result["command_ok"], false);
        assert_eq!(result["outcome_class"], "partial_failure");
        assert_eq!(result["execution_lane"], "async_batch");
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn exec_many_parallel_runs_independent_commands_concurrently() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        #[cfg(windows)]
        let sleep = json!({"program": "powershell", "args": ["-NoProfile", "-Command", "Start-Sleep -Milliseconds 1000"]});
        #[cfg(unix)]
        let sleep = json!({"program": "sh", "args": ["-c", "sleep 1"]});

        let started = std::time::Instant::now();
        let result = call_tool_async(
            ctx,
            "exec_many".into(),
            json!({
                "mode": "parallel",
                "max_parallel": 2,
                "stop_on_error": false,
                "commands": [sleep.clone(), sleep]
            }),
        )
        .await;

        assert_eq!(result["all_commands_ok"], true, "{result}");
        assert_eq!(result["mode"], "parallel");
        let batch_elapsed_ms = started.elapsed().as_millis() as u64;
        let individual_elapsed_ms = result["results"]
            .as_array()
            .expect("batch results")
            .iter()
            .filter_map(|item| item["result"]["elapsed_ms"].as_u64())
            .sum::<u64>();
        assert!(
            individual_elapsed_ms > batch_elapsed_ms.saturating_add(500),
            "parallel commands did not overlap enough: {result}"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn exec_many_lock_group_serializes_shared_resources() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        #[cfg(windows)]
        let sleep = json!({"program": "powershell", "args": ["-NoProfile", "-Command", "Start-Sleep -Milliseconds 500"], "lock_group": "cargo-target"});
        #[cfg(unix)]
        let sleep =
            json!({"program": "sh", "args": ["-c", "sleep 0.5"], "lock_group": "cargo-target"});

        let started = std::time::Instant::now();
        let result = call_tool_async(
            ctx,
            "exec_many".into(),
            json!({
                "mode": "parallel",
                "max_parallel": 2,
                "stop_on_error": false,
                "commands": [sleep.clone(), sleep]
            }),
        )
        .await;

        assert_eq!(result["all_commands_ok"], true, "{result}");
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(900),
            "{result}"
        );
        let max_resource_wait = result["results"]
            .as_array()
            .expect("batch results")
            .iter()
            .filter_map(|item| item["resource_lock_wait_ms"].as_u64())
            .max()
            .unwrap_or(0);
        assert!(max_resource_wait >= 400, "{result}");
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn exec_many_dag_skips_failed_dependencies_but_runs_independent_work() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let result = call_tool_async(
            ctx,
            "exec_many".into(),
            json!({
                "mode": "dag",
                "max_parallel": 4,
                "stop_on_error": false,
                "commands": [
                    {"id": "fail", "program": "coding-tools-command-that-does-not-exist"},
                    {"id": "blocked", "depends_on": ["fail"], "program": "cargo", "args": ["--version"]},
                    {"id": "independent", "program": "cargo", "args": ["--version"]}
                ]
            }),
        )
        .await;

        assert_eq!(result["successful_command_count"], 1, "{result}");
        assert_eq!(result["failed_command_count"], 1, "{result}");
        assert_eq!(result["skipped_command_count"], 1, "{result}");
        assert_eq!(result["failed_command_ids"], json!(["fail"]), "{result}");
        assert_eq!(
            result["skipped_command_ids"],
            json!(["blocked"]),
            "{result}"
        );
        assert_eq!(result["results"][1]["id"], "blocked");
        assert_eq!(result["results"][1]["skip_reason"], "dependency_failed");
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn exec_command_redacts_sensitive_file_output_before_transport() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        std::fs::write(
            workspace.path().join("profiles.json"),
            "bare-secret-without-label",
        )
        .expect("write sensitive fixture");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        #[cfg(windows)]
        let command = "cmd /d /c type profiles.json";
        #[cfg(unix)]
        let command = "sh -c \"cat profiles.json\"";

        let result = call_tool_async(
            ctx,
            "exec_command".into(),
            json!({
                "cmd": command,
                "timeout_ms": 10_000,
                "yield_time_ms": 10_000,
                "output_mode": "tail"
            }),
        )
        .await;

        assert_eq!(result["command_ok"], true, "{result}");
        assert_eq!(result["stdout"], "[REDACTED]", "{result}");
        assert_eq!(result["sensitive_data_redacted"], true, "{result}");
        assert!(!result.to_string().contains("bare-secret-without-label"));
    }

    #[tokio::test]
    async fn format_files_plan_routes_without_modifying_workspace() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        std::fs::write(workspace.path().join("data.json"), "{\"b\":2,\"a\":1}\n")
            .expect("write json fixture");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        let result = call_tool_async(
            ctx,
            "format_files".into(),
            json!({"paths": ["data.json"], "mode": "plan"}),
        )
        .await;

        assert_eq!(result["ok"], true, "{result}");
        assert_eq!(result["status"], "planned", "{result}");
        assert_eq!(
            result["groups"][0]["adapter_id"], "builtin-json",
            "{result}"
        );
        assert_eq!(result["applied"], false, "{result}");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("data.json")).expect("read fixture"),
            "{\"b\":2,\"a\":1}\n"
        );
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn retained_exec_preserves_the_wait_command_next_action() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let ctx = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );

        #[cfg(windows)]
        let command = "powershell -NoProfile -Command \"Start-Sleep -Seconds 30\"";
        #[cfg(unix)]
        let command = "sh -c \"sleep 30\"";

        let result = call_tool_async(
            ctx.clone(),
            "exec_command".into(),
            json!({
                "cmd": command,
                "deduplicate": true,
                "timeout_ms": 60_000,
                "yield_time_ms": 0,
                "output_mode": "none"
            }),
        )
        .await;

        assert_eq!(result["process_still_running"], true, "{result}");
        assert_eq!(
            result["next_actions"][0]["tool"], "wait_command",
            "{result}"
        );
        assert_eq!(
            result["next_actions"][0]["arguments"]["session_id"], result["session_id"],
            "{result}"
        );

        let reattached = call_tool_async(
            ctx.clone(),
            "exec_command".into(),
            json!({
                "cmd": command,
                "deduplicate": true,
                "timeout_ms": 60_000,
                "yield_time_ms": 0,
                "output_mode": "none"
            }),
        )
        .await;
        assert_eq!(
            reattached["session_id"], result["session_id"],
            "{reattached}"
        );
        assert_eq!(reattached["deduplicated"], true, "{reattached}");
        assert_ne!(
            reattached["harness_operation_id"], result["harness_operation_id"],
            "{reattached}"
        );

        let harness_operation_ids = [
            result["harness_operation_id"]
                .as_str()
                .expect("harness operation id")
                .to_string(),
            reattached["harness_operation_id"]
                .as_str()
                .expect("reattached harness operation id")
                .to_string(),
        ];
        let session_id = result["session_id"].as_str().expect("session id");
        let killed = call_tool_async(
            ctx.clone(),
            "kill_session".into(),
            json!({"session_id": session_id, "wait_ms": 10_000}),
        )
        .await;
        assert_eq!(killed["killed"], true, "{killed}");

        let operations = ctx.harness.list_operations(0, 20).expect("operation log");
        for operation_id in &harness_operation_ids {
            let correlated = operations
                .iter()
                .filter(|operation| operation.id == *operation_id)
                .collect::<Vec<_>>();
            assert_eq!(
                correlated
                    .iter()
                    .map(|operation| operation.kind.as_str())
                    .collect::<Vec<_>>(),
                vec!["started", "failed"]
            );
        }
        let terminal = operations
            .iter()
            .find(|operation| {
                operation.id == harness_operation_ids[0] && operation.kind == "failed"
            })
            .expect("terminal operation");
        assert_eq!(terminal.result_summary["command_ok"], false);
        assert_eq!(terminal.result_summary["termination_reason"], "killed");
        assert!(terminal.result_summary.get("command").is_none());
        assert!(terminal.result_summary.get("stdout").is_none());
    }
}
