use std::fs;

use coding_tools_mcp_desktop_lib::tools::{call_tool, ToolContext};
use serde_json::json;

#[test]
fn 无任务时仍可执行_dry_run_预检() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "dry_run": true,
            "patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+预检内容\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["preflight"], true);
    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(
        fs::read_to_string(temp.path().join("workspace/README.md")).unwrap(),
        "初始内容\n"
    );
}

#[test]
fn codex_patch格式支持新增文件dry_run和实际应用() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let patch = "*** Begin Patch\n*** Add File: probe.txt\n+probe-v2\n*** End Patch\n";

    let dry_run = call_tool(
        &ctx,
        "apply_patch",
        &json!({"patch": patch, "dry_run": true}),
    );
    assert_eq!(dry_run["ok"], true);
    assert_eq!(dry_run["dry_run"], true);
    assert!(dry_run["affected_files"]
        .as_array()
        .expect("影响文件")
        .iter()
        .any(|file| file["path"] == "probe.txt" && file["operation"] == "add"));
    assert!(!workspace.join("probe.txt").exists());

    let applied = call_tool(&ctx, "apply_patch", &json!({"patch": patch}));
    assert_eq!(applied["ok"], true);
    assert_eq!(
        fs::read_to_string(workspace.join("probe.txt")).expect("读取新增文件"),
        "probe-v2\n"
    );
}

#[test]
fn 无任务时普通_patch也可执行并保留撤销能力() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "apply_patch",
        &json!({
            "patch": "--- a/README.md\n+++ b/README.md\n@@\n-初始内容\n+已修改\n"
        }),
    );

    assert_eq!(result["ok"], true);
    assert_eq!(result["harness_mode"], "standalone");
    assert!(!result
        .as_object()
        .unwrap()
        .contains_key("pre_change_snapshot_id"));
    assert_eq!(
        fs::read_to_string(workspace.join("README.md")).unwrap(),
        "已修改\n"
    );

    let log = call_tool(&ctx, "operation_log", &json!({}));
    assert_eq!(log["ok"], true);
    assert!(log["operations"]
        .as_array()
        .expect("操作日志")
        .iter()
        .any(|operation| operation["tool"] == "apply_patch"));
}

#[test]
fn 无任务时_exec_command不返回任务门禁错误() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "git status", "filesystem_scope": "workspace"}),
    );

    assert_ne!(result["error"]["code"], "TASK_STATE_REQUIRED");
    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(result["execution_mode"], "direct");
    assert_eq!(result["task_required"], false);
    assert_eq!(result["command"], "git status");
    assert_eq!(result["status"], "exited");
    assert!(result["exit_code"].is_i64() || result["exit_code"].is_u64());
    assert!(result["duration_ms"].is_u64());
    assert_eq!(result["duration_ms"], result["elapsed_ms"]);
    assert_eq!(result["next_actions"], json!([]));
    assert!(result["recovery_hint"].is_string());
    assert!(!result
        .as_object()
        .unwrap()
        .contains_key("pre_change_snapshot_id"));
}

#[test]
fn 无任务时_exec错误不应建议启动任务() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "python -c \"import sys; sys.exit(1)\""}),
    );

    assert_eq!(result["harness_mode"], "standalone");
    assert_eq!(result["task_required"], false);
    assert_eq!(result["next_actions"], json!([]));
    assert!(result["recovery_hint"].is_string());
    if let Some(actions) = result["harness"]["next_actions"].as_array() {
        assert!(!actions.iter().any(|action| action == "start_task"));
    }
}

#[test]
fn workspace_allows_exec_during_transition() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let result = call_tool(&ctx, "exec_command", &json!({"cmd": "python --version"}));

    assert_ne!(result["error"]["code"], "EXEC_SANDBOX_UNAVAILABLE");
    assert_eq!(result["execution_mode"], "direct");
    assert_eq!(result["filesystem_scope"], "workspace");
    assert_eq!(result["sandbox_enforced"], false);
}

#[test]
fn harness_tools_support_task_lifecycle() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");

    let started = call_tool(
        &ctx,
        "start_task",
        &json!({"objective": "补齐 Harness 状态"}),
    );
    assert_eq!(started["ok"], true);
    let task_id = started["task"]["id"].as_str().expect("任务 ID");

    let updated = call_tool(
        &ctx,
        "update_task",
        &json!({"task_id": task_id, "pending_steps": ["接入门禁"]}),
    );
    assert_eq!(updated["ok"], true);
    let context = call_tool(&ctx, "task_context", &json!({}));
    assert_eq!(context["ok"], true);
    assert_eq!(context["task"]["id"], task_id);
    assert!(!context["events"].as_array().expect("事件").is_empty());

    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "allow_unverified": true}),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["task"]["status"], "completed_unverified");
}

#[test]
fn finish_task摘要持久化且change_id选择不可变快照() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    let harness_root = temp.path().join("harness");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace.clone(), harness_root.clone()).expect("创建上下文");

    let started = call_tool(&ctx, "start_task", &json!({"objective": "保存完成证据"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    let blank = call_tool(
        &ctx,
        "finish_task",
        &json!({"task_id": task_id, "summary": "   ", "allow_unverified": true}),
    );
    assert_eq!(blank["ok"], false);
    assert_eq!(blank["error"]["code"], "INVALID_ARGUMENT");
    assert_eq!(
        ctx.harness
            .current_task()
            .expect("读取任务")
            .expect("活动任务")
            .status,
        coding_tools_mcp_desktop_lib::harness::TaskStatus::Active
    );

    fs::write(workspace.join("README.md"), "已发布\n").expect("修改工作区");
    let finished = call_tool(
        &ctx,
        "finish_task",
        &json!({
            "task_id": task_id,
            "summary": "  已保存发布证据  ",
            "allow_unverified": true
        }),
    );
    assert_eq!(finished["ok"], true);
    assert_eq!(finished["summary"], "已保存发布证据");
    let change_id = finished["change_id"].as_str().expect("变更 ID");
    assert_eq!(change_id.len(), 32);
    assert_eq!(finished["task"]["latest_change_id"], change_id);
    assert_eq!(finished["change_summary"]["change_id"], change_id);
    assert_eq!(
        finished["change_summary"]["why"],
        json!({"text": "已保存发布证据", "source": "finish_task_summary"})
    );
    let captured_files = finished["change_summary"]["files"]
        .as_array()
        .expect("变更文件")
        .clone();
    assert!(captured_files
        .iter()
        .any(|file| file["path"] == "README.md" && file["status"] == "modified"));
    assert!(finished["change_summary"]["evidence"]
        .as_array()
        .expect("证据")
        .iter()
        .any(|event| {
            event["kind"] == "task_finished"
                && event["reason"]["text"] == "已保存发布证据"
                && event["result_summary"]["change_id"] == change_id
        }));

    fs::write(workspace.join("README.md"), "后续外部修改\n").expect("后续修改");
    let selected = call_tool(&ctx, "change_summary", &json!({"change_id": change_id}));
    assert_eq!(selected["ok"], true);
    assert_eq!(selected["files"], json!(captured_files));
    let latest = call_tool(&ctx, "change_summary", &json!({"task_id": task_id}));
    assert_eq!(latest["files"], selected["files"]);

    let malformed = call_tool(&ctx, "change_summary", &json!({"change_id": "../bad"}));
    assert_eq!(malformed["ok"], false);
    assert_eq!(malformed["error"]["code"], "INVALID_ARGUMENT");

    let restarted = ToolContext::for_test(workspace.clone(), harness_root).expect("重启上下文");
    let restored = call_tool(
        &restarted,
        "change_summary",
        &json!({"change_id": change_id}),
    );
    assert_eq!(restored["ok"], true);
    assert_eq!(restored["files"], selected["files"]);
    assert_eq!(restored["why"], selected["why"]);

    let next = call_tool(
        &restarted,
        "start_task",
        &json!({"objective": "下一个任务"}),
    );
    let mismatch = call_tool(
        &restarted,
        "change_summary",
        &json!({"task_id": next["task"]["id"], "change_id": change_id}),
    );
    assert_eq!(mismatch["ok"], false);
    assert_eq!(mismatch["error"]["code"], "CHANGE_TASK_MISMATCH");

    let fallback = call_tool(
        &restarted,
        "finish_task",
        &json!({"task_id": next["task"]["id"], "allow_unverified": true}),
    );
    assert_eq!(fallback["ok"], true);
    assert_eq!(fallback["summary"], "下一个任务");
    assert_eq!(
        fallback["change_summary"]["why"],
        json!({"text": "下一个任务", "source": "task_objective"})
    );
}

#[test]
fn task_context遵守max_bytes预算() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx = ToolContext::for_test(workspace, temp.path().join("harness")).expect("创建上下文");
    let objective = "bounded-context-".repeat(2_000);

    let started = call_tool(&ctx, "start_task", &json!({"objective": objective}));
    assert_eq!(started["ok"], true);
    let context = call_tool(&ctx, "task_context", &json!({"max_bytes": 8_192}));

    assert_eq!(context["ok"], true);
    assert_eq!(context["max_bytes"], 8_192);
    assert_eq!(context["truncated"], true);
    assert!(serde_json::to_vec(&context).expect("序列化上下文").len() <= 8_192);
    assert!(
        context["task"]["objective"]
            .as_str()
            .expect("任务目标")
            .len()
            < "bounded-context-".repeat(2_000).len()
    );
}

#[test]
fn 外部修改会在写工具执行前被拒绝() {
    let temp = tempfile::tempdir().expect("创建临时目录");
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace).expect("创建工作区");
    fs::write(workspace.join("README.md"), "初始内容\n").expect("写入文件");
    let ctx =
        ToolContext::for_test(workspace.clone(), temp.path().join("harness")).expect("创建上下文");
    let started = call_tool(&ctx, "start_task", &json!({"objective": "检查外部变化"}));
    let task_id = started["task"]["id"].as_str().expect("任务 ID");
    fs::write(workspace.join("README.md"), "外部修改\n").expect("模拟外部修改");

    let result = call_tool(
        &ctx,
        "exec_command",
        &json!({"cmd": "git status", "filesystem_scope": "workspace"}),
    );

    assert_eq!(result["ok"], false);
    assert_eq!(result["error"]["code"], "FILE_CHANGED_EXTERNALLY");
    assert_eq!(
        ctx.harness
            .current_task()
            .expect("读取任务")
            .expect("活动任务")
            .id,
        task_id
    );
}

#[test]
fn 工具清单包含项目状态和任务上下文能力() {
    let tools = coding_tools_mcp_desktop_lib::tools::list_tools_for_profile("advanced");
    let names = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    for expected in [
        "project_state",
        "start_task",
        "task_context",
        "list_task_events",
        "change_summary",
    ] {
        assert!(names.contains(&expected), "缺少工具 {expected}");
    }
    assert!(!names.contains(&"undo_last_patch"));
}
