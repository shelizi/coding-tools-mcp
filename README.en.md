<p align="center">
  <img src="src-tauri/icons/128x128.png" width="96" alt="Coding Tools MCP icon">
</p>

<h1 align="center">Coding Tools MCP</h1>

<p align="center">
  Turn a local project into a persistent AI development workspace that carries context across conversations.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Release-see%20releases-blue" alt="Releases">
  <img src="https://img.shields.io/badge/Windows-x64-0078D4?logo=windows" alt="Windows x64">
  <img src="https://img.shields.io/badge/macOS-Apple%20Silicon-000000?logo=apple" alt="macOS Apple Silicon">
  <a href="https://www.apache.org/licenses/LICENSE-2.0"><img src="https://img.shields.io/badge/license-Apache--2.0-blue" alt="Apache-2.0"></a>
</p>

<p align="center">
  <a href="README.md">繁體中文</a> · <a href="README.en.md">English</a> · Releases
</p>

Coding Tools MCP now ships in two forms: a Desktop Client (Rust + Tauri 2) and a portable Node Agent. They share the same Svelte management UI and Workspace model. After selecting project folders and starting MCP, an AI agent can read and edit files, run commands and tests, operate Git, and store durable history inside the project. On Windows, Computer Use also provides display discovery, screenshots, click, drag, scroll, text input, and keyboard input.

The same logical Workspace can use a protected canonical configuration and secrets store across Desktop and Node while preserving host-specific fields. Node Agent remains a headless/portable, MCP-only product; Desktop-only capabilities such as Actions and FRP/Cloudflare process management stay in the Desktop Client.

![Coding Tools MCP workspace overview](docs/images/workspace-overview.png)

*Desktop and Node Agent share the same management UI. Public endpoints and local paths in this screenshot are covered with solid redaction.*

## Current highlights

| Capability | Current behavior |
| --- | --- |
| **Shared management UI** | Desktop Client and Node Agent use the same Svelte UI for Workspace, MCP, health, telemetry, operation history, and settings |
| **Shared Workspace** | Canonical configuration and secrets use a protected shared store; host-specific fields remain separate and the same folder set can keep one logical Workspace identity |
| **Skills / Hooks / external MCP** | Skills, Claude/Codex Hooks, and external MCP servers can be discovered and enabled individually; Node Agent can proxy stdio and Streamable HTTP MCP |
| **Computer Use (Windows)** | `desktop_displays`, `desktop_screenshot`, `desktop_click`, `desktop_drag`, `desktop_scroll`, `desktop_type`, and `desktop_key`, using physical-pixel coordinates |
| **Execution safety** | Workspace-first boundaries, sensitive-output redaction, Git protection, and optional command sandboxes that fail closed when enabled |

## Understand the workflow in 30 seconds

```text
Install the desktop app
  → sidebar Quick setup (or add a workspace manually)
  → choose a tunnel source and finish connection
  → start MCP and copy the Public MCP URL
  → enable ChatGPT developer mode
  → create an MCP plugin and paste the URL
  → authorize it and start developing in a new conversation
```

For a first connection, remember only this: **the Desktop Client or Node Agent turns the project into an MCP Workspace, and the AI client connects through an `/mcp` URL.** Use the Desktop Client when you need Actions, FRP/Cloudflare process management, or other Desktop-only integrations.

- [See the complete desktop setup](#get-started-in-five-minutes)
- [Quick setup wizard](#2-quick-setup-recommended-for-first-use)
- [Go directly to the ChatGPT plugin setup](#mcp-connector)

## Get started in five minutes

### 1. Install the desktop client

Download the package for your platform from this repository's Releases page:

| Platform | Package |
| --- | --- |
| Windows 10/11 x64 | `Coding.Tools.MCP_*_x64-setup.exe` |
| macOS Apple Silicon | `Coding Tools MCP_*_aarch64.dmg` |

The macOS build is currently unsigned. If macOS blocks the first launch, allow it from System Settings → Privacy & Security.

### 2. Quick setup (recommended for first use)

Open **Quick setup** in the sidebar (route `/quick-setup`) and follow the wizard. The flow matches the app implementation in five steps:

| Step | What you do |
| --- | --- |
| **1. Tunnel source** | Choose **Built-in WSS** (recommended), **FRP**, or **Cloudflare** |
| **2. Project** | Name the workspace (blank = first folder name), pick one or more project folders, create the workspace |
| **3. Connection type** | Choose **MCP** or **Actions** (the wizard enables one service at a time) |
| **4. Enable** | Fill source-specific fields, test the tunnel, start the service |
| **5. Finish** | See the exact values to paste into ChatGPT (public URL, auth fields) with copy helpers |

**What step 4 needs, by tunnel source:**

| Source | You provide | The wizard does |
| --- | --- | --- |
| **Built-in WSS** | A **one-time enrollment link** from your server admin (`https://…/_tunnel/enroll/<code>`, HTTPS required) | Validates the link, registers the local device key, updates the public URL |
| **FRP** | FRP server host/port and optional token | Checks/installs `frpc`; select or create a global FRP profile; MCP uses `/clients/<id>/mcp`, Actions needs its own subdomain |
| **Cloudflare Quick** | No token | Checks/installs `cloudflared`; a temporary `trycloudflare.com` URL is created during the connection test |
| **Cloudflare Named** | Tunnel token + fixed HTTPS public URL | Checks/installs `cloudflared`; saves token and fixed URL |

Afterwards you can open the normal workspace UI for advanced options (auth, policies, running MCP and Actions together). **Quick setup does not** create server-side enrollment links, an FRPS instance, or a Cloudflare Named Tunnel for you—those remain server/console tasks.

For self-hosted built-in WSS, see [3a. Built-in WSS](#3a-built-in-wss-self-hosted-server) and [`services/tunnel-server/README.md`](services/tunnel-server/README.md).

### 3. Add a workspace manually (advanced)

Without the wizard:

1. Click **Add workspace** in the sidebar.
2. Select the project root directory.
3. Configure the workspace name, MCP port, and authentication mode.
4. Save it. The workspace remains available in the sidebar across conversations and restarts.

### 4. Configure a public tunnel

When the AI client is not on the same machine, expose MCP as a publicly reachable HTTPS URL. Each workspace can use one of three tunnel types (**new workspaces default to built-in WSS**):

| Type | When to use | What you do in the desktop app |
| --- | --- | --- |
| **Built-in WSS (`builtin`)** | Self-host `coding-tools-tunnel-server`; terminate TLS with Caddy (or similar) | Paste the one-time enrollment link from the server |
| **FRP** | You already run FRPS | Save server/token under **FRP settings**; set a subdomain per workspace |
| **Cloudflare** | Cloudflare Tunnel | Install/detect `cloudflared`; set a token or use Quick Tunnel |

#### 3a. Built-in WSS (self-hosted server)

Architecture (matches the current implementation):

```text
ChatGPT / public HTTPS clients
        → Caddy (TLS)
        → coding-tools-tunnel-server (8088 public / optional 8089 Admin)
        → desktop embedded WSS client (protocol coding-tools-tunnel-v3)
        → local MCP or Actions
```

**Server (summary)**

1. Start with Docker Compose or the binary as described in [`services/tunnel-server/README.md`](services/tunnel-server/README.md) ([繁體中文](services/tunnel-server/README.md) / [English](services/tunnel-server/README.en.md)).
2. Set `CODING_TOOLS_TUNNEL_PUBLIC_ORIGIN` to your real HTTPS origin (enrollment links use it).
3. If Admin WebUI is enabled: supply username and password yourself (**the server never auto-generates the password**; at least 12 bytes).
4. Create a one-time enrollment link via Admin or CLI (for example `enroll create --client-id <id> --service both`).

Reverse-proxy these paths to port `8088` (ahead of any FRP fallback): `/_tunnel/v1`, `/_tunnel/enroll/*`, `/builtin/*`, and the matching `/.well-known/.../builtin/*` routes. Do **not** expose Admin (`8089`) on the public internet.

**Desktop**

1. Set the workspace tunnel type to **Built-in WSS tunnel**.
2. Paste the one-time enrollment link and save.
3. The app generates an Ed25519 keypair and device ID locally, stores the **private key** in the OS secret store, and registers only the **public key**. The server-assigned Client ID is authoritative; the public URL updates automatically.
4. MCP and Actions in the same workspace share one device identity. After revocation, paste a **new** enrollment link to rotate keys.

Public paths look like:

- MCP: `https://<your-domain>/builtin/clients/<client-id>/mcp`
- Actions: `https://<your-domain>/builtin/actions/<client-id>`

Local placeholders default to `http://127.0.0.1:8088/...` (same default origin as the server). Production should use your HTTPS reverse-proxied domain.

Full deploy steps, env vars, CLI, and troubleshooting: [tunnel-server README (zh-TW)](services/tunnel-server/README.md) / [English](services/tunnel-server/README.en.md). Design notes: [`docs/builtin-wss-tunnel.md`](docs/builtin-wss-tunnel.md).

#### 3b. FRP

- Install or detect `frpc` from **Software management**.
- Save the server, port, and token under **FRP settings**.
- Give each workspace a distinct subdomain. The app manages the FRP process and aggregates multiple proxy routes.

![FRP configuration](docs/images/frp-configuration.png)

*FRP server profiles are stored centrally; each workspace only selects a profile and supplies its own subdomain.*

If you do not have an FRPS server yet, follow this [FRPS server installation guide (Chinese, WeChat)](https://mp.weixin.qq.com/s/kmpQhHsvmHlaLfj4rw3A0Q). After deployment, enter the server address, port, and token under **FRP settings** in the desktop client.

#### 3c. Cloudflare

- Install or detect `cloudflared` from **Software management**.
- Select Cloudflare in the workspace and configure a named-tunnel token or Quick Tunnel.

### 5. Start MCP

Open a Workspace and start MCP. The management UI brings together:

- a local MCP URL such as `http://127.0.0.1:28766/mcp`;
- the public HTTPS MCP URL and ChatGPT connection settings;
- OAuth/authentication, policy, and health entry points;
- telemetry, operation history, and Workspace settings.

![MCP service and local connection settings](docs/images/workspace-connection.png)

*Public MCP, OAuth, and other identifiable connection data remain available in the MCP page; the README screenshot intentionally keeps only a safe loopback example and does not expose the real endpoint.*

Use **Health** to validate local/public endpoints and OAuth metadata. **Telemetry** and **Operation history** help trace tool calls, latency, results, and failures. The Desktop Client also retains Desktop-specific runtime and tunnel diagnostics.

### 6. Connect an AI client

Use the public MCP URL shown by the app. With OAuth enabled, the client follows the server metadata into the authorization flow; authorization codes, Client IDs, and secrets can be generated and managed from the desktop client. This release uses preconfigured OAuth clients, so select static/manual OAuth credentials when creating a ChatGPT plugin; CIMD is not required.

For a first connection, ask the agent to initialize history before inspecting the workspace:

```text
history_session_bootstrap
server_info
get_default_cwd
git_status
check_exec_environment
```

This gives the agent explicit project and capability state instead of guessing from the current chat window.

## Two ways to connect ChatGPT

| Mode | Best for | Use this endpoint |
| --- | --- | --- |
| MCP Connector | Direct access to files, commands, and Git | the workspace's public `/mcp` URL |
| GPT Actions | Importing OpenAPI tools into a custom GPT | the Actions panel's `/openapi.json` URL |

### MCP Connector

Before configuring ChatGPT, make sure that:

1. The workspace MCP service and public tunnel are both running.
2. The public MCP endpoint passes the desktop health check. If OAuth is enabled, also verify the protected-resource document and authorization metadata.
3. You have copied the **Public MCP URL** from the desktop **GPT configuration** card. For OAuth, also have the OAuth Client ID, OAuth Client Secret, and authorization password ready.

> ChatGPT must use the public HTTPS `/mcp` URL. A local address such as `http://127.0.0.1:28766/mcp` is not reachable from ChatGPT. Menu names may vary slightly by ChatGPT version and language.

#### 1. Enable ChatGPT developer mode

Open ChatGPT settings, go to **Account security and sign-in**, and enable **Developer mode**. This allows unverified MCP connectors to be added.

![Enable developer mode in ChatGPT](docs/images/gpt-config-1.png)

*Developer mode grants powerful access. Only connect MCP servers that you operate or explicitly trust.*

#### 2. Create the MCP plugin

Open **Plugins** from the ChatGPT sidebar, click the `+` button, select the MCP beta option, and enter:

| ChatGPT field | Value |
| --- | --- |
| Name | A recognizable name such as `Coding Tools MCP` |
| Description | A short description of the connected project or purpose |
| Connection | The public MCP URL from the desktop **GPT configuration** card; it should end in `/mcp` |
| Authentication | The same mode configured in the desktop app; the screenshot uses OAuth |

![Create an MCP plugin and enter its connection details](docs/images/gpt-config-2-detail.png)

For OAuth, open the advanced OAuth settings, select static/manual OAuth credentials, and enter the Client ID and Client Secret shown by the desktop app. CIMD is not required. When ChatGPT opens the authorization page, enter the authorization password from the desktop **GPT configuration** card.

> Client Secrets, authorization passwords, and Bearer tokens are sensitive. Never paste them into chats, issues, or public screenshots. If the desktop app uses Bearer or no authentication, select the matching option currently offered by ChatGPT.

#### 3. Verify the connection

Start a new conversation with the plugin enabled and ask:

```text
Use Coding Tools MCP to call server_info, get_default_cwd, and git_status.
Tell me which workspace is connected, its default directory, and its Git status.
```

If ChatGPT returns information from the current project, the desktop app, public tunnel, authentication, ChatGPT, and MCP tool chain are connected end to end. Before real development, call `history_session_bootstrap` to initialize or restore project history.

If ChatGPT still shows an old tool list, disconnect and reconnect the plugin or verify again in a new conversation.

#### Troubleshooting

| Symptom | Check first |
| --- | --- |
| ChatGPT cannot connect | Confirm that the URL is the public HTTPS `/mcp` endpoint rather than `127.0.0.1`, and that the public MCP health check passes |
| OAuth authorization fails | Confirm that the Client ID, Client Secret, and authorization password come from the same workspace, and check the OAuth metadata results |
| New tools are missing | Disconnect and reconnect the plugin, then start a new conversation |
| A tool call fails | Open **Logs** and **Health checks** in the desktop app and confirm that the request reached the MCP service |

### GPT Actions

1. Start the workspace Actions service.
2. Copy the OpenAPI URL from the Actions panel.
3. Import the URL in the GPT editor's Actions page.
4. Select None, API Key, or OAuth to match the desktop configuration.

MCP and Actions can run together for the same workspace, with separate ports and subdomains when needed.

## Why use it

- **Built for real development**: files, commands, Git, tests, and retained processes live in one Workspace.
- **Cross-conversation continuity**: a new conversation can recover the complete history summary and the latest detailed handoff.
- **Auditable progress**: structured checkpoints preserve decisions, changed files, test results, remaining issues, and next steps inside the project.
- **Multiple workspaces and runtimes**: Desktop Client and Node Agent share the management UI and Workspace model; Desktop manages MCP, Actions, and the full tunnel integration while Node Agent focuses on MCP.
- **Direct ChatGPT connectivity**: Streamable HTTP, OAuth, Bearer tokens, OpenAPI, plus built-in WSS, FRP, and Cloudflare tunnels.
- **Extensible agent behavior**: Skills, Hooks, and external MCP servers can be discovered, grouped, and enabled individually instead of being permanently active.
- **Desktop interaction**: Windows Computer Use can capture the screen and drive mouse, scroll, text, and keyboard input for real GUI validation.
- **A focused default tool surface**: stable core tools are available by default; advanced Harness capabilities are opt-in.

## Let the project remember every conversation

Chat transcripts are useful for rereading a discussion, but they are a poor long-term development handoff. Coding Tools MCP stores progress in `docs/history-session/` under the current project, so context follows the repository instead of staying trapped in one chat window.

![ChatGPT new-conversation startup prompt](docs/images/history-session-prompt.png)

*Paste the full prompt into a new conversation to initialize or restore history, then save a checkpoint after each completed task.*

Three tools work together:

| Tool | Purpose |
| --- | --- |
| `history_session_bootstrap` | Initialize or restore a project session; a new file embeds a compressed summary of prior sessions and returns a stable `session_key` and `current_path` |
| `history_session_checkpoint` | Save structured progress to the stable target returned by bootstrap; reject mismatched targets instead of writing to another history file |
| `history_session_validate` | Validate numbering, history files, and session mappings; rebuild derived indexes when needed without deleting existing history |

History uses readable Markdown that can be backed up or committed with the project. Every new file starts with a bounded inherited summary that is not recursively copied into later summaries. Checkpoints are idempotent, and progress should only be reported as saved after the tool returns `ok=true` with the same session target.

> History persistence is performed when the AI calls the MCP tools; the desktop app does not record chat content in the background. If the client does not invoke a tool, the server cannot infer that a new conversation or task has happened.

## What an agent can do

The available tools depend on the selected profile and runtime. Common development tools include:

| Category | Main tools |
| --- | --- |
| File reading | `read_file`, `read_many`, `list_files`, `search_text`, `view_image` |
| File modification | `apply_patch`, `edit`, `file_ops`, `format_files` |
| Commands and sessions | `exec_command`, `exec_many`, `wait_command`, `send_input`, `kill_session` |
| Git | `git_status`, `git_diff`, `git_log`, `git_show`, `git_blame`, `git_stage`, `git_commit` |
| Workspace routing | `list_workspace_folders`, `switch_workspace_folder`, `set_default_cwd` |
| History sessions | `history_session_bootstrap`, `history_session_checkpoint`, `history_session_validate` |
| Computer Use (Windows) | `desktop_displays`, `desktop_screenshot`, `desktop_click`, `desktop_drag`, `desktop_scroll`, `desktop_type`, `desktop_key` |

A typical development loop is:

```text
Open Workspace
  → understand project and Git state
  → search and read code
  → apply a transactional patch
  → run commands and tests
  → inspect the diff and commit
```

The advanced profile retains project-state and operation-history Harness capabilities, but normal edits and command execution do not require a Task.

## Permission and recovery model

The project uses a Workspace-first permission model:

- Normal files inside the Workspace can be read, created, modified, deleted, and executed.
- Outside the Workspace, `read_file`, `list_dir`, `list_files`, `search_text`, and `view_image` provide read-only access.
- Writes, deletes, and command execution outside the Workspace are blocked.
- `.git` and `.github` cannot be damaged through ordinary file tools, Patch, or interpreter commands.
- Patch performs preflight validation and operation-local recovery; long-term recovery uses Git instead of full Workspace snapshots.

> Command sandboxing is a workspace setting and is off by default. While disabled, the honest execution boundary remains `policy_only` with `sandbox_enforced: false`. When enabled, commands must use the selected OS sandbox and never fall back to policy-only execution: Windows host folders can use AppContainer (network is denied by default; set `appcontainer.network=internet` to allow it); WSL folders should use Docker, Podman, Docker Sandboxes (sbx), or WSL Containers. Native Docker / Podman run ephemeral Linux containers with network `none` by default and reject `host` networking. Install or detect `docker`, `podman`, `sbx`, and `wslc` from **Software management**; starting the engine or `podman machine` remains a user-run step. File-tool workspace read access remains a separate boundary from the command sandbox.

## Local development

Requirements: Node.js 20+, Rust stable, and the [Tauri 2 prerequisites](https://v2.tauri.app/start/prerequisites/) for your platform.

```bash
pnpm install --frozen-lockfile
pnpm run hooks:install
pnpm run desktop
```

`hooks:install` points this clone's `core.hooksPath` at the tracked `.githooks` directory. On commit, fully staged Rust files are formatted and re-staged automatically. If a staged Rust file also has unstaged hunks, the hook stops without modifying the index so unrelated changes cannot be pulled into the commit accidentally.

Useful verification commands:

```bash
pnpm run check
pnpm run build
cd src-tauri && cargo test
cd src-tauri && cargo clippy --all-targets -- -D warnings
```

On Windows, you can also run `dev-desktop.cmd`. Do not use `pnpm run dev` alone to validate the desktop application; it starts Vite without the Tauri shell.

## Project layout

| Path | Purpose |
| --- | --- |
| `src-tauri/src/tools/` | Shared file, Patch, Exec, and Git tool kernel |
| `src-tauri/src/mcp/` | MCP Streamable HTTP server |
| `src-tauri/src/actions/` | ChatGPT Actions OpenAPI gateway |
| `src-tauri/src/tunnel/` | Built-in WSS, FRP, and Cloudflare tunnels and process management |
| `services/tunnel-server/` | Self-hosted built-in WSS tunnel server (Rust) |
| `src/` | Shared Svelte management UI used by Desktop Client and Node Agent |
| `packages/node-agent/` | Portable/headless Node Agent, management HTTP API, and shared-UI adapter |
| `old/` | Python reference implementation and compatibility baseline |

## Source repository and attribution

This project is a derivative of the upstream Coding Tools MCP work and preserves its copyright and license notices (see [NOTICE](NOTICE)):

- [xyTom/coding-tools-mcp](https://github.com/xyTom/coding-tools-mcp)
- [mybolide/coding-tools-mcp](https://github.com/mybolide/coding-tools-mcp)

## Acknowledgments

Thanks to the [Linux.do](https://linux.do/) community for project promotion and feedback.

## License

This project is licensed under the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0).

- Full license text: [LICENSE](LICENSE)
- Attribution / NOTICE: [NOTICE](NOTICE)

If you use code, documentation, substantial implementation details, or derivative work from this project, preserve the copyright notice, license notice, and `NOTICE` file, and clearly attribute the original project.
