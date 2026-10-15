# Conversation handle verification

Desktop 0.1.63 and Node Agent 0.29.46 share the authenticated conversation-handle
contract. Handles are memory-only, bound to the verified credential and issuance
identity, expire after 24 hours, and become invalid after server restart.

## Development validation

- Rust MCP regression: 92 passed, 2 existing ignored tests.
- Node focused handle, HTTP, response, streaming and redaction regression: 29 passed.
- Responses expose complete tool data and handle metadata in one text block while
  preserving images/resources and error status. Legacy text-only errors retain
  their text; protocol Task envelopes without tool content stay unchanged.
- Tests cover credential/native-identity conflicts, expiry/capacity/restart,
  folder/cwd continuation, process controls, and routing-token secrecy even when
  general telemetry redaction is disabled.
- Release preflight repaired the split-v5 test fixture: route and desired
  connection assertions now use their fixture constants, ready data connections
  are awaited, and teardown closes tunnel sockets before the HTTP server.
- The tunnel suite passed all 21 tests and completed cleanup naturally.
- Complete Node verify:repo passed: 616 tests, 608 passes, 8 skips, zero failures.
- Full parity execution passed all 28 assertions. The Windows test setup import
  uses a file URL so the runner can load it on Windows and Unix.
- Version, native boundary, generated revision and UI parity checks passed.

Earlier development checks encountered timing failures and the tunnel fixture
hang; the release preflight results above supersede those historical limits.
Public CI results and release assets are available in Actions and Releases.

## Platform compatibility

Gemini reported successful handle-backed folder switching and directory listing,
but its adapter exposed only a trailing metadata text block. Version 0.1.63 merges
the complete structured payload and handle metadata into a single text block.
Models must carry the handle on subsequent built-in tool calls; this does not
establish platform-level conversation isolation. Bootstrap again after restart
or credential refresh, and refresh client tool definitions if new arguments are
not visible. Verify client-side rendering after updating the server.
