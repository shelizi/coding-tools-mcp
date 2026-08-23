use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::OnceLock;
use std::thread;

use serde_json::{json, Value};

use crate::tunnel::append_profile_log_rotating;

const TOOL_USAGE_LOG_FILE: &str = "mcp-tool-usage.jsonl";
const TOOL_USAGE_LOG_MAX_BYTES: u64 = 20 * 1024 * 1024;
const TOOL_USAGE_LOG_RETAINED_FILES: usize = 5;
const TOOL_USAGE_LOG_QUEUE_CAPACITY: usize = 1_024;

type ToolUsageLogEntry = (String, String);

static TOOL_USAGE_LOG_SENDER: OnceLock<Option<SyncSender<ToolUsageLogEntry>>> = OnceLock::new();
static TOOL_USAGE_LOG_DROPPED: AtomicU64 = AtomicU64::new(0);

fn tool_usage_log_sender() -> Option<&'static SyncSender<ToolUsageLogEntry>> {
    TOOL_USAGE_LOG_SENDER
        .get_or_init(|| {
            let (sender, receiver) =
                sync_channel::<ToolUsageLogEntry>(TOOL_USAGE_LOG_QUEUE_CAPACITY);
            let worker = thread::Builder::new()
                .name("mcp-tool-usage-log".into())
                .spawn(move || {
                    while let Ok((profile_id, line)) = receiver.recv() {
                        append_profile_log_rotating(
                            &profile_id,
                            TOOL_USAGE_LOG_FILE,
                            &line,
                            TOOL_USAGE_LOG_MAX_BYTES,
                            TOOL_USAGE_LOG_RETAINED_FILES,
                        );
                    }
                });
            worker.ok().map(|_| sender)
        })
        .as_ref()
}

pub(super) fn append_tool_usage_log(profile_id: &str, mut record: Value) {
    let dropped_before = TOOL_USAGE_LOG_DROPPED.swap(0, Ordering::Relaxed);
    if dropped_before > 0 {
        if let Some(object) = record.as_object_mut() {
            object.insert("telemetry_dropped_before".into(), json!(dropped_before));
        }
    }

    let Ok(line) = serde_json::to_string(&record) else {
        TOOL_USAGE_LOG_DROPPED.fetch_add(dropped_before.saturating_add(1), Ordering::Relaxed);
        return;
    };

    let Some(sender) = tool_usage_log_sender() else {
        append_profile_log_rotating(
            profile_id,
            TOOL_USAGE_LOG_FILE,
            &line,
            TOOL_USAGE_LOG_MAX_BYTES,
            TOOL_USAGE_LOG_RETAINED_FILES,
        );
        return;
    };

    match sender.try_send((profile_id.to_string(), line)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            TOOL_USAGE_LOG_DROPPED.fetch_add(dropped_before.saturating_add(1), Ordering::Relaxed);
        }
        Err(TrySendError::Disconnected((profile_id, line))) => {
            append_profile_log_rotating(
                &profile_id,
                TOOL_USAGE_LOG_FILE,
                &line,
                TOOL_USAGE_LOG_MAX_BYTES,
                TOOL_USAGE_LOG_RETAINED_FILES,
            );
        }
    }
}
