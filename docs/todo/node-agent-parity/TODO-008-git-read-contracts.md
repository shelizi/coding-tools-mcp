<!-- parity-id: NP-008 -->
<!-- parity-status: done -->
# NP-008 — Git read-tool response contract parity

- Priority: P1
- Area: git-contract
- Status: done


## Gap

Resolved in Node Agent 0.17.0. Git read tools now expose the Rust-compatible repository, tracking, structured file, normalization, truncation, revision, and blame-line contracts while preserving Node's stronger mutation rollback behavior.

## Rust evidence

- `src-tauri/src/tools/git.rs`

## Node current state

- `packages/node-agent/src/gitTools.ts`
- `packages/node-agent/test/gitReadContracts.test.mjs`

## Implementation scope

The five Git read tools now use workspace-safe paths, validated revisions, non-interactive Git subprocesses, bounded output, and Rust-compatible structured response fields. Existing branch, stage, commit, and rollback-protected restore behavior is unchanged.

## Acceptance checklist

- [x] git_status reports repository, branch, tracking, clean, head, and rename metadata.
- [x] git_diff honors both `staged` and `unstaged` and returns structured files.
- [x] git_log includes short hash, path/ref, warnings, and look-ahead truncation.
- [x] git_show returns content/files and normalized context metadata.
- [x] git_blame returns structured line records rather than raw porcelain text.

## Verification

Covered by a shared repository/bare-remote fixture with diverged tracking branches, staged and unstaged changes, renames, path filtering, byte/context limits, revision validation, structured blame lines, non-repository behavior, and the existing mutation/restore regression suite.
