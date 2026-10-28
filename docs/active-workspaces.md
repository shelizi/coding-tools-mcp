# Active Coding MCP workspace/worktree access

`list_active_workspaces` reports projects and linked Git worktrees that had **successful MCP tool calls** recently.

- **active**: last successful tool call within **3 minutes** (default).
- **recent**: last call between **3 and 5 minutes** ago (default).
- Older: not included in the response. A configured but untouched project is never presented as active.
- Optional `active_within_minutes` and `recent_within_minutes` are integers from 1 to 60; recent must be at least active.
- Project results include name, folder ID, path, distinct conversation count, tool-call count, and last-access timestamp. Checkout entries identify a normal repository or linked worktree by its `.git` marker.

Example:

```json
{"active_within_minutes":3,"recent_within_minutes":5}
```

This is a **read-only runtime-local observation** for one Coding MCP Agent process, not a global lock, a heartbeat, or proof another Agent is still running. It cannot see agents that bypass this MCP runtime, and does not yet aggregate separate Node and Rust processes. A running command whose Agent makes no further tool calls can age out. Agent and conversation identifiers are kept internal; only counts are returned.

Do not use this tool alone to make destructive or exclusive Git operations safe. Those still require existing Git guards and user confirmation.
