# Server-managed Worker Pool Tasks

- [x] Add protocol v3 worker-policy serialization and validation tests.
- [x] Add policy-store default, persistence, revision, and validation tests.
- [x] Add authenticated Admin API and static UI contract tests.
- [x] Add server handshake, live-update, and route-cap integration tests.
- [x] Add desktop bootstrap, grow, delayed scale-down, and graceful recycle tests.
- [x] Remove workspace-owned worker-count settings.
- [x] Implement protocol, policy store, Admin API/UI, and server enforcement.
- [x] Implement the desktop dynamic pool manager and telemetry.
- [x] Split worker acquisition from response-head timeout and bound pending queues.
- [x] Add explicit 503 capacity responses, demand hints, connecting-capacity grace, staged shrink, and burst warm retention.
- [x] Make `exec_many` use bounded safe auto scheduling with inferred shared-resource locks.
- [x] Report repeated sequential execution opportunities in tool-usage telemetry.
- [x] Record sanitized command-pair overlap, conflict, and lock-serialization outcomes.
- [x] Feed Wilson-confidence pair statistics back into `exec_many mode=auto`.
- [x] Expose explainable parallel decisions and LLM recommendations; defer ML until data matures.
- [x] Add Admin queue metrics/policy controls and a reusable public MCP load-test script.
- [x] Run protocol, server, desktop, frontend, formatting, clippy, and GitNexus checks.

Protocol and tunnel-server Clippy pass with warnings denied. The full desktop
Clippy command still reports pre-existing lint debt in unrelated runtime/tool
modules; desktop compilation and all library tests pass.
