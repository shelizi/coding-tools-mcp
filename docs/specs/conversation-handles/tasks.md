# Tasks

## 交付物清单
Server stores and ingress adapters, shared schemas, regression tests, client documentation.

## 任务列表

- [x] Implement bounded, expiring authenticated handle stores in Rust and Node.
- [x] Add authenticated ingress preparation and response decoration; guard reserved
      native key prefixes; preserve permission/path routing and strip arguments.
- [x] Publish MCP tool parameters and bootstrap instructions; omit from Actions.
- [x] Protect raw handle fields and token strings in redaction/telemetry.
- [x] Synchronize generated Rust catalog and Node revisions.
- [x] Run store, HTTP, process continuation, native compatibility, negative security,
      schema and telemetry tests on D:; run version/parity/verify checks and report
      pre-existing failures accurately.
- [x] Document client usage, TTL/credential refresh/restart behavior and limitations.

## 需求覆盖矩阵
FR-1, FR-2, FR-3: stores, ingress and schema tasks; FR-4, FR-5, FR-6: negative and expiry tests; FR-7: redaction and telemetry tests; FR-8: catalog sync and Actions schema; FR-9: compatibility regressions.

## 文件变更清单
See design.md file structure. Specs and Node Agent README document the contract.

## 验证结果
See [verification.md](verification.md) for completed checks, baseline failures and deployment status.
