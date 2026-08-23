mod apply_ops;
mod edit_ops;
mod file_ops;
mod hunk;
mod parser;
mod precise_edit;
mod proposal;
mod support;
mod transaction;

use serde_json::Value;

use crate::tools::context::ToolContext;
use crate::tools::workspace::WorkspaceError;

pub fn apply_patch(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    apply_ops::run_patch(ctx, args)
}

pub fn patch_check(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    apply_ops::run_check(ctx, args)
}

pub fn edit_file(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    edit_ops::run_file(ctx, args)
}

pub fn edit(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    edit_ops::run_edit(ctx, args)
}

pub fn edit_many(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    edit_ops::run_many(ctx, args)
}

pub fn file_ops(ctx: &ToolContext, args: &Value) -> Result<Value, WorkspaceError> {
    file_ops::run(ctx, args)
}

#[cfg(test)]
mod tests;
