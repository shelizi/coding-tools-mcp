# Conversation handle verification

Verified on 2026-10-15 using the synchronized D: workspace
`D:\coding-tools-mcp-build\worktrees\gemini-routing`. All build, test and log
outputs stayed on D:. Initial implementation checks ran before the release
activation recorded below. No commit was performed.

## Passed

- Rust MCP regression: 90 passed, 2 ignored. This includes the initial three
  handle-store tests and authenticated HTTP routing/credential-conflict test.
- Rust hub: 10 passed; registry/schema: 8 passed; Actions/OpenAPI: 6 passed;
  redaction: 6 passed.
- Final Rust handle-store suite: 4 passed, adding invalid-value rejection,
  argument stripping, response decoration and selective Tasks suppression.
- Node package regression excluding the previously hanging `tunnel.test.mjs`:
  586 passed, 8 skipped, zero failures (594 total).
- The four new Node handle tests cover expiry/capacity/restart, authenticated
  HTTP folder/cwd continuation, credential and native identity rejection,
  native OpenAI opt-in, telemetry/persistence secrecy, and process continuation.
- Node server build, Rust catalog equality, generated revisions, version check,
  parity roadmap structure/evidence validation, UI parity checker and native
  package boundary check passed.
- Spec validation: 9 requirements, zero errors/warnings. `git diff --check` passed.

Logs are under `D:\coding-tools-mcp-build\tmp\handle-*.log`.

## Existing verification limits

The full Node verifier previously hung in the existing split-v5 tunnel test.
The successful broad regression above deliberately excludes that file; it is
not a successful unmodified `node-agent:verify-repo` run. Roadmap validation here
used `--structure-only`, rather than claiming every behavioral assertion executed.

Additional root-level static tool/UI contract tests produced 8 passes and three
failures. All three were confirmed against the original HEAD:

- The catalog has 63 tools, while the test expects 62.
- The test searches for an obsolete literal `nodeClean` implementation string.
- The existing UI checklist heading has an encoding mismatch with its test.

These unrelated files were not changed to conceal baseline failures.

## Deployment status

Rust Desktop 0.1.62 was packaged with `custom-protocol` and activated successfully
at 2026-10-15 23:30:59 Asia/Taipei. Process 4020 runs the new artifact; old process
16096 was replaced. All previously enabled services were restored (one MCP
profile, zero Actions), and no rollback was required. The local discovery endpoint
and an authenticated OpenAI connector `server_info` call both returned 0.1.62;
the live compat-readonly-all revision is `dc3a1b5adc9bccf5`.

The current connector's cached bootstrap schema still lacks the new handle
arguments. The native bootstrap request succeeded, but this session could not
verify handle opt-in/continuation through that stale connector schema. Reconnect
or refresh the client tool definitions before the live Gemini/OpenAI handle test.
Client models still need to carry the returned handle; this does not establish
platform-level conversation isolation. Node Agent source is synchronized, but
no running Node Agent was replaced by this Desktop activation.

Package: `D:\coding-tools-mcp-build\artifacts\conversation-handles-0.1.62\ctmcp-0.1.62-win64.zip`

- ZIP size: 11,281,704 bytes.
- ZIP SHA-256: `97CBB472693EFB3C83545C333C3B625875D74A63C6C3528DBC7E7A1AD16E0811`.
- Executable SHA-256: `0803369F338AFF48734839AC0B1E72813F26EF548C090D01785B02CEC60F9958`.
- Activation evidence: `D:\coding-tools-mcp-build\artifacts\conversation-handles-0.1.62\activation-result.json`.

## Single-text response compatibility fix

Gemini reported that its adapter exposed only the trailing metadata text block,
omitting the tool's primary structured result. The Rust and Node handle decorators
now serialize the complete merged structured result and handle metadata into one
text block, while retaining image/resource blocks and the original error status.
Legacy text-only tool errors retain their text; protocol Task envelopes without
tool content remain unchanged. Decoration still occurs after execution telemetry.

- Rust MCP regression: 92 passed, 2 existing ignored tests.
- Node focused handle/HTTP/response/streaming/redaction regression: 29 passed.
- Tests assert matching text/structured payloads, directory/file data, bootstrap
  handle delivery, image/resource preservation, error preservation, legacy text,
  Task envelope compatibility, and routing-token secrecy with redaction disabled.
- Version check and parity roadmap structure/evidence check passed for 0.1.63.
- Updated spec check: zero errors/warnings.

Source validation is complete; release activation is recorded separately below.

### 0.1.63 activation

Activated at 2026-10-15 23:57:23 Asia/Taipei. Process 66316 runs the packaged
0.1.63 executable, and both local discovery and authenticated OpenAI connector
`server_info` report 0.1.63. The existing one MCP profile was restored, with zero
Actions services. Node source was fixed and tested; a Node Agent runtime was not replaced.

The first handoff rolled back after Windows PowerShell 5 treated a child-process
termination error as fatal. Retrying the same staged artifact with PowerShell 7
succeeded. Evidence for both attempts is retained in the artifact directory.

Package: `D:\coding-tools-mcp-build\artifacts\conversation-handles-0.1.63\ctmcp-0.1.63-win64.zip`

- ZIP size: 11,303,876 bytes.
- ZIP SHA-256: `ADAFBD030D9A8632F615B9748CD708236869AC4F6EA87CB12283A9BCBCE89E69`.
- Executable SHA-256: `AF767C51EF3D3B8FA8DF7EF6A1E003CA4E22CFA9E30C2161294BC526C391190B`.
- Activation evidence: `D:\coding-tools-mcp-build\artifacts\conversation-handles-0.1.63\activation-result.json`.

The Gemini-side response rendering still needs a fresh platform call after
activation. Previous handles are invalid after restart: bootstrap again first.

## Pre-commit verification (2026-10-16)

- `version:check`, native boundary check, generated revision check and UI parity
  checker passed. The Rust catalog equality preflight also passed.
- The required `node-agent:verify-repo` run built the complete Node package and
  reported 587 passes, 8 skips and 3 failures. Two process timing assertions
  (`managedJobLifecycle` deadline metadata and modern MCP Tasks completion)
  passed when rerun in isolation. The remaining tunnel worker hung and was
  explicitly stopped after the other workers completed; the verifier did not pass.
- The full parity runner exposed an invalid Windows absolute-path `--import`.
  Converting the test setup path with `pathToFileURL(...).href` fixed loading.
  Its retest passed 27 of 28 assertions. The only remaining failure was the same
  hanging tunnel test; only that owned test worker was stopped to collect results.
- Fresh GitNexus staged-change analysis covered 35 intended files and 32 affected
  flows. Its aggregate rating was CRITICAL because MCP ingress, listener lifecycle
  and workspace routing are shared entry points. The changed-file scope and the
  Rust/Node ingress contexts were reviewed; unrelated changes remained unstaged.
  The parity runner repair separately had LOW impact with two direct callers.

This commit does not claim a clean full verifier/parity run. Test logs and graph
reports are retained under `D:\coding-tools-mcp-build\tmp\handle-commit-*`.

## Public release preflight repair (2026-10-16)

The previously hanging split-v5 test registered route 23 but asserted route 41.
It also asserted three desired connections although its own capacity message
requested one; three actual connections were independently created under stream
pressure. The assertions now use their fixture constants and wait for ready
connections. A single teardown stops the manager, terminates WebSockets, closes
the WebSocket server, then closes HTTP, so assertion failures cannot deadlock the
earlier HTTP-close hook. This repair changes test code only.

Validation on the sanitized public snapshot:

- Tunnel suite: 21 passed; the earlier failing assertion also completed cleanup
  instead of hanging during diagnosis.
- Complete `node-agent:verify-repo`: 616 tests, 608 passed, 8 skipped, zero failures.
- Rust catalog equality, version, native boundary, full parity (28/28), and UI
  parity (7/7) gates all passed.
- Frontend `check`: zero errors and warnings.

The release preflight supersedes the historical tunnel limitation above. Logs
are retained under `D:\coding-tools-mcp-build\public-sync\0.1.63-1dcd4c7`.
