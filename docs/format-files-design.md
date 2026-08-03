# Generic `format_files` design

## Purpose

`format_files` is a multi-language formatting coordinator. It detects applicable formatters, builds a bounded execution plan, runs external tools in an isolated mirror, and applies verified output back to the workspace only in `apply` mode.

The implementation is built on a reusable file-action core so future `lint_files`, `fix_files`, and import-organization tools can share project detection, adapter selection, policy checks, execution isolation, diff generation, and guarded writes without combining their public semantics.

## Public request

```json
{
  "paths": ["src/main.rs", "web/App.tsx", "config/settings.json"],
  "scope": "files",
  "mode": "plan",
  "formatter": "auto",
  "strict": false,
  "include_patterns": ["src/**"],
  "exclude_patterns": ["**/generated/**"],
  "max_files": 500,
  "timeout_ms": 120000,
  "expected_sha256": {
    "src/main.rs": "<64-character SHA-256>"
  },
  "confirm": false,
  "max_diff_bytes": 262144
}
```

### Modes

- `plan`: detect files, adapters, configuration, grouping, and risks without running a formatter.
- `check`: run formatters in an isolated mirror and return bounded diffs without modifying the workspace.
- `apply`: run in the mirror, verify all guards, then write formatted output to the workspace.

### Scopes

- `files`: explicit files or directories.
- `changed`: Git staged, unstaged, and untracked files.
- `staged`: Git index changes only.
- `project`: bounded project traversal.

Project-wide apply and apply requests exceeding the broad-change threshold require `confirm=true`.

## Adapter selection

Selection is deterministic and uses this priority:

1. Explicit `formatter` requested by the caller.
2. A matching workspace custom adapter.
3. The nearest supported formatter configuration.
4. A formatter declared in a project manifest.
5. A language or file-type default.

Manifest detection reads relevant declarations instead of selecting solely by filename:

- `pyproject.toml` distinguishes Ruff and Black configuration.
- `package.json` inspects dependencies and scripts for Biome, dprint, or Prettier.

When multiple custom adapters support the same extension in automatic mode, planning returns `FORMATTER_AMBIGUOUS` and requires explicit selection.

## Built-in adapters

The initial adapter registry supports:

- Rust: rustfmt
- JavaScript, TypeScript, JSONC, CSS, HTML, Markdown and related frontend files: Biome, dprint, or Prettier
- Python: Ruff Format or Black
- Go: gofmt
- C, C++, Java and Protocol Buffers: clang-format
- C#: CSharpier
- Kotlin: ktfmt or ktlint
- Shell: shfmt
- Terraform and HCL: terraform fmt
- TOML: Taplo
- JSON without a project formatter: the built-in JSON formatter

Generated dependency manifests and lockfiles are skipped instead of being rewritten by a generic formatter. Binary and unsupported files are also reported as skipped.

## Isolated execution

External formatters never receive direct write access to the selected workspace files during formatting.

1. Read selected files and record SHA-256 values.
2. Create a bounded mirror under `.coding-tools-format/<operation-id>`.
3. Copy only selected files and required nearby configuration or manifest files.
4. Run each adapter with a structured executable plus argument list; no shell command string is generated.
5. Snapshot the mirror before and after each adapter group.
6. Reject changes outside that adapter's selected files with `FORMAT_UNEXPECTED_CHANGES`.
7. Produce bounded unified diffs.
8. In `apply` mode, re-read every target and compare its current SHA-256 with the preflight value.
9. Write outputs and roll back already-written files if a later write fails.
10. Remove the mirror.

This is process isolation by controlled inputs, paths, environment, and workspace policy. It is not an operating-system sandbox, so broad or unknown tools are not treated as trusted automatically.

## Missing formatters

- Non-strict requests skip files whose selected formatter is unavailable and return a warning.
- Strict requests fail with `FORMATTER_UNAVAILABLE`.
- The coordinator never installs packages or downloads formatters.

Workspace-local tools are preferred over system tools. Known locations include project package binaries, virtual environments, `bin`, and `tools` directories.

## Workspace custom adapters

Custom adapters are read from `.coding-tools/formatters.json`:

```json
{
  "formatters": {
    "company-template": {
      "program": "tools/template-format",
      "extensions": ["tmpl"],
      "args": ["--write", "{files}"],
      "config": "config/template-format.json"
    }
  }
}
```

Constraints:

- Adapter IDs use only letters, numbers, `.`, `_`, and `-`.
- IDs cannot replace built-in adapters.
- `program` and optional `config` must be workspace-relative and cannot contain parent traversal.
- Arguments are a structured array, not a shell command.
- Supported placeholders are `{files}`, `{file}`, `{config}`, and `{workspace}`.
- `{files}` or `{file}` is required.
- Unknown placeholder syntax is rejected.
- `plan` is always safe to preview.
- `check` and `apply` with a custom adapter require `confirm=true`.
- A custom program containing a path separator is resolved only inside the workspace and is never searched on the host PATH.

## Result and telemetry

The result reports:

- requested, supported, changed, unchanged, and skipped counts
- adapter groups and selection sources
- custom adapter flags
- unavailable adapters
- unexpected changes
- bounded diff and truncation state
- whether changes were applied

Tool-usage schema version 5 records bounded aggregate format metrics, including mode, scope, changed/skipped counts, adapter group counts, custom group counts, unavailable adapters, unexpected changes, diff bytes, and apply status. File contents and full diffs are not written to telemetry.

## TDD coverage

The implementation is driven by tests for:

- safe request defaults and validation
- multi-language grouping
- nearest configuration and manifest-aware selection
- generated, binary, and unsupported file handling
- explicit formatter overrides
- Git changed and staged scopes
- structured shell-free adapter commands
- check isolation and apply behavior
- SHA mismatch rejection
- unexpected mirror change rejection
- formatter availability behavior in strict and non-strict modes
- workspace-local executable preference
- custom adapter planning, path rejection, and confirmation
- shared dispatch, policy, registry schema, and telemetry contracts
