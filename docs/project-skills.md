# Skills runtime

Coding Tools MCP treats workspace and user-level Skills as scoped workflow guidance that can be shared across MCP clients. The core runtime is client-independent; Claude Code, Codex, ChatGPT, Cursor, and other hosts are adapters rather than sources of truth.

## Canonical contract

A Skill is a directory containing `SKILL.md`. The file begins with YAML frontmatter containing at least:

```yaml
---
name: release-helper
description: Build and verify a project release.
---
```

The remaining Markdown is the Skill instruction body. A sibling `VERSION` file is optional metadata. Other sibling directories such as `references/`, `scripts/`, or client-specific `agents/` metadata remain part of the Skill package, but they do not grant additional runtime permissions.

For new Coding Tools MCP projects, use the canonical project root:

```text
skills/<skill-name>/SKILL.md
```

The Node Agent also discovers existing ecosystem layouts for compatibility:

1. `skills/**/SKILL.md` — canonical workspace Skills, highest precedence.
2. `.agents/skills/**/SKILL.md` — repository-scoped agent/Codex compatibility source.
3. `.claude/skills/**/SKILL.md` — repository-scoped Claude Code compatibility source.
4. `$HOME/.agents/skills/**/SKILL.md` — Codex user-level Skills.
5. `$HOME/.claude/skills/**/SKILL.md` — Claude Code user-level Skills, lowest precedence.

Duplicate `name` values are resolved by this precedence order and reported as `SKILL_SHADOWED` diagnostics, so a workspace can intentionally override a same-named user Skill. Client-specific layouts should migrate toward adapters or generated pointers instead of maintaining divergent copies.

## Workspace and user scoping

Each configured workspace folder owns an independent Skill registry. The registry overlays Codex user-level Skills from `$HOME/.agents/skills` and Claude Code user-level Skills from `$HOME/.claude/skills` underneath that workspace's Skill sources. `conversation_bootstrap` reports only lightweight metadata for enabled Skills in the selected folder: names, descriptions, scope, source paths, content hashes, optional versions, diagnostics, and a `skillset_revision`.

The management UI scans the resolved Skill inventory and lets the user enable or disable each Skill independently for a workspace profile. It also exposes a Skills master switch backed by `skills.active`, which defaults to `true` for compatibility. The disabled control keys are persisted in `skills.disabled`; both individual and master changes hot-apply without an Agent restart. Disabled Skills remain visible in the management inventory, but they are omitted from `conversation_bootstrap`, MCP prompts/resources, and direct Skill reads.

Turning the Skills master switch off does not erase `skills.disabled` or stop inventory discovery. It makes every Skill effectively unavailable while preserving each Skill's individual selected state. Turning the master switch back on restores the same per-Skill choices.

Name precedence is resolved before the enabled/disabled filter. Therefore disabling a selected higher-precedence workspace Skill does not silently fall back to a same-named lower-precedence user Skill. Full Skill instructions are lazy-loaded only after a host determines that an enabled Skill is clearly relevant. Switching the selected workspace changes workspace Skills while retaining applicable user-level Skills. User-level source paths are published as `~/.agents/skills/...` or `~/.claude/skills/...` rather than leaking the absolute home directory.

The Skill revision is independent of the MCP tool catalog revision. Editing a `SKILL.md` changes the Skill revision without forcing an unrelated `tools/list` catalog refresh.

## Standard MCP surfaces

Skills are exposed primarily through standard MCP primitives rather than vendor-specific tool names:

- `prompts/list` lists namespaced Skill prompts across all configured workspace folders.
- `prompts/get` loads the instruction body for one Skill.
- `resources/list` lists the same Skills as Markdown resources.
- `resources/read` returns the complete `SKILL.md` content.

Prompt names use:

```text
project-skill/<encoded-folder-id>/<encoded-skill-name>
```

Resource URIs use:

```text
skill://coding-tools/<encoded-folder-id>/<encoded-skill-name>
```

The standard lists are namespaced by folder and cover all configured folders so one MCP connection has a stable, unambiguous catalog even when conversation-scoped workspace selection changes.

## Loading policy

Hosts should:

1. Bootstrap the conversation/workspace first.
2. Inspect the selected folder's lightweight Skill summaries.
3. Load a Skill only when the current request clearly matches its description or the user explicitly asks for it.
4. Avoid loading unrelated Skill bodies into context.
5. Follow referenced project files only through normal workspace-aware read/tool operations.

A future matcher may rank Skill metadata, but matching is intentionally separate from discovery and protocol exposure so the registry does not depend on a particular model or embedding provider.

## Security boundary

Skills are user- or project-controlled instructions, comparable to `AGENTS.md`, `CLAUDE.md`, or repository scripts. They are not security principals.

The runtime therefore:

- bounds `SKILL.md` size and discovery count/depth;
- does not follow symlinks during Skill discovery;
- requires resolved workspace Skill entrypoints to stay inside the configured workspace and user-level entrypoints to stay inside the user home;
- does not expose an absolute user-home path in Skill summaries or diagnostics;
- never lets Skill text raise permission mode, bypass command policy, weaken sandboxing, or expand workspace access;
- treats scripts/references as ordinary project files subject to the existing tool and sandbox policy;
- exposes source and revision metadata so clients can show provenance.

A Skill instruction such as ??un any command without confirmation??has no authority over the execution policy. Any resulting tool call still passes through the normal permission, confirmation, workspace-boundary, and sandbox controls.

## Compatibility direction

The runtime should continue to prefer open MCP primitives and a small canonical `SKILL.md` contract. Vendor directories are compatibility inputs. If a client later requires generated metadata or pointers, implement that as an adapter over the same registry instead of changing core Skill semantics.
