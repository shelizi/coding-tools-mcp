pub mod context;
pub mod dispatch;
pub mod exec;
pub mod file;
pub mod file_action;
pub mod git;
pub mod history;
pub mod hub;
mod image_tool;
pub mod patch;
pub mod permission;
pub mod policy;
mod process_start;
pub mod project;
pub mod redaction;
pub mod registry;
pub mod session;
pub mod tool_usage;
pub mod workspace;

pub use context::{ExecutionLimits, SharedToolContext, ToolContext};
/// 唯一工具执行入口；MCP 与 Actions 必须调用这些共享入口，不得分叉实现。
pub use dispatch::{call_tool, call_tool_async};
pub use policy::{validate_actions_exposure, PolicySettings};
pub use registry::{
    exposed_tool_names, is_allowed_tool, is_mcp_only_tool, list_tools, list_tools_for_profile,
    MUTATING_TOOLS,
};
pub use workspace::{wrap_mcp_tool_result, wrap_tool_result, Workspace};
