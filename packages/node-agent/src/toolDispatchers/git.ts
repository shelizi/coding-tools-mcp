import {
  gitBlameTool,
  gitBranchTool,
  gitWorktreeTool,
  gitCommitTool,
  gitDiffTool,
  gitLogTool,
  gitPushTool,
  gitRestoreTool,
  gitShowTool,
  gitStageTool,
  gitStatusTool
} from '../gitTools.js';
import type { ToolHandlerMap } from '../toolDispatch/contract.js';

export const gitToolHandlers = {
  git_status: ({ ctx, key, args }) => gitStatusTool(ctx, key, args),
  git_diff: ({ ctx, key, args }) => gitDiffTool(ctx, key, args),
  git_log: ({ ctx, key, args }) => gitLogTool(ctx, key, args),
  git_show: ({ ctx, key, args }) => gitShowTool(ctx, key, args),
  git_blame: ({ ctx, key, args }) => gitBlameTool(ctx, key, args),
  git_branch: ({ ctx, key, args }) => gitBranchTool(ctx, key, args),
  git_worktree: ({ ctx, key, args }) => gitWorktreeTool(ctx, key, args),
  git_stage: ({ ctx, key, args }) => gitStageTool(ctx, key, args),
  git_commit: ({ ctx, key, args }) => gitCommitTool(ctx, key, args),
  git_push: ({ ctx, key, args }) => gitPushTool(ctx, key, args),
  git_restore: ({ ctx, key, args }) => gitRestoreTool(ctx, key, args)
} satisfies ToolHandlerMap;
