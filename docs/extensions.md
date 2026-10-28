# Coding MCP-owned Hooks and MCP servers

Coding Tools MCP owns its extension definitions. The Node Agent and Rust Desktop read only
the Coding MCP namespace; **they do not discover or import Claude, Codex, or other
agents' MCP, Hook, or Skill configurations**.

## Configuration locations

| Scope | MCP servers | Hooks |
| --- | --- | --- |
| Per workspace folder | `.coding-tools-mcp/mcp.json` | `.coding-tools-mcp/hooks.json` |
| Per OS user | `~/.coding-tools-mcp/mcp.json` | `~/.coding-tools-mcp/hooks.json` |

`~` is the account running the MCP process, **not** the remote ChatGPT/Claude
client's home directory. Workspace definitions override same-named user
definitions for that workspace. All enabled/disabled selections are scoped to
the Coding MCP workspace profile, not a host Agent.

The older `.mcp.json`, `.claude.json`, `.claude/**`, `.codex/**`, and
`.agents/**` paths are **not inputs** to extension discovery. No implicit
migration or copying takes place.

### MCP example

Create `.coding-tools-mcp/mcp.json`:

```json
{
  "mcpServers": {
    "local-demo": {
      "type": "stdio",
      "command": "node",
      "args": ["path/to/your/mcp-server.js"]
    },
    "remote-demo": {
      "type": "http",
      "url": "https://your-trusted-server.example/mcp",
      "enabled": false
    }
  }
}
```

Both `stdio` and streamable `http` transports are supported. The
`enabled: false` field prevents a source from being enabled at all; it is
separate from Coding MCP's **opt-in** enable switch. Optional fields include
`cwd`, `env`, `env_vars`, `headers`, `env_http_headers`, and
`bearer_token_env_var`. Use environment-variable references or protected
local files for credentials; avoid committing secrets. Be careful with commands
in untrusted workspace files: selecting a server explicitly starts that
executable with the Agent's privileges.

### Hook example

Create `.coding-tools-mcp/hooks.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "read_file",
        "hooks": [
          { "type": "command", "command": "node", "args": ["./scripts/your-hook.mjs"] }
        ]
      }
    ]
  }
}
```

Supported events: `SessionStart`, `SessionEnd`, `PreToolUse`,
`PostToolUse`, and `PostToolUseFailure`. Hook handlers support `command`
and `http`. Unsupported events/handlers remain discoverable but disabled.

## Lifecycle and explicit trust

Discovery never executes a Hook, spawns a process, or connects to an HTTP MCP
endpoint. In Coding MCP's own management UI, open **Skills / Hooks / MCP**
and opt in to the individual entries you trust. Each category has a separate
master switch; switching it off preserves individual selections.

When an MCP server is selected, Coding MCP initializes it, obtains
`tools/list`, and adds namespaced tools to its own MCP tool catalog. The
toolset revision changes, so client applications may need to reconnect or
refresh their MCP tool list. Deselecting a server closes its connection.

Read-only permission mode disallows proxied external MCP calls. MCP responses,
secrets and URLs receive the same existing management-UI privacy handling.
Current UI controls support inventory, enable/disable, connection status, and
tool counts; add/update/remove definitions by editing the **Coding MCP-owned**
JSON files above and then refreshing. Do not edit other Agents' configuration
files. This applies to both Node Agent and Rust Desktop.

## Migration from older releases

Previously-selected servers and Hooks from `.mcp.json` or other Agents do not
automatically reappear under the new namespace. To keep one, explicitly copy
only its **trusted configuration** into the matching Coding MCP-owned
`mcp.json` or `hooks.json`, reload discovery, and select the new entry.
Old opaque selection keys are intentionally not reused. Existing external
Agent settings are never changed or deleted by this migration.

For Skill sources and precedence, see [project-skills.md](project-skills.md).
