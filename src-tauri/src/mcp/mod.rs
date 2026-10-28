mod auto_activity;
mod conversation_handles;
mod listener;
mod server;
mod session_activity;
pub(crate) mod session_expiry;
mod tasks;
mod telemetry;

pub(crate) use listener::behavioral_parity_fixture;
pub use listener::{spawn_listener, ShutdownSender};
pub use server::{LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS};
pub(crate) use session_activity::{
    background_job_refs as session_background_job_refs, now_ms as session_activity_now_ms,
    recent_actions as session_recent_actions, snapshot as session_activity_snapshot,
    workspace_last_access_ms, ACTIVE_WINDOW_MS as SESSION_ACTIVE_WINDOW_MS,
};
pub(crate) use telemetry::{
    classify_command_text, command_kind, legacy_compat_write_enabled, legacy_compat_write_env,
    record_async_session_finalized, record_diagnostic_event, runtime_boot_id,
    tool_usage_log_health, AsyncSessionTelemetry,
};
mod identity_diagnostics;
