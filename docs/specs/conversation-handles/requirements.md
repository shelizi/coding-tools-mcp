# Conversation handles

## 功能概述
Gemini's observed MCP requests have no conversation metadata or session headers.
Explicit workspace selectors work, but persistent selection needs a server-issued
handle carried in tool arguments. Rust Desktop and Node Agent must behave alike.
This does not establish platform-level chat isolation or cross-platform sharing.

## 需求列表
FR-1: The server SHALL issue an unpredictable handle for an authenticated MCP
   conversation_bootstrap without native identity or an existing handle, and use an isolated
   server conversation key. Return it even when folder selection is required.
FR-2: The server SHALL preserve existing native-identity routing and issue an optional
   handle when bootstrap receives create_conversation_handle=true.
FR-3: The server SHALL reuse selection/cwd for a valid handle on any built-in MCP tool. Remove
   the argument before tool execution, deduplication, history and telemetry.
FR-4: The server SHALL reject a handle from another verified credential, another profile, a different
   native identity (including present versus absent), or an expired/unknown handle
   without falling back to a native/default/explicit route.
FR-5: The server SHALL expire handles after 24 hours (absolute), store them only in memory, and invalidate them after
   process restart. Limit live handles to 1024 per profile/runtime. Expiry releases
   handle-owned selections and saved cwd without explicitly terminating commands.
FR-6: The server SHALL reject handle creation/use without verified authentication. Existing native
   and explicit workspace routing remain available subject to existing policy.
FR-7: The server SHALL strip routing handle arguments before logging or persistence. Only bootstrap
   returns the raw handle; handle-backed responses include expiry and source information.
   No fixed default conversation fallback is allowed.
   Text consumers SHALL receive the complete merged tool data and handle metadata
   in one text block; non-text content and error status SHALL be preserved.
FR-8: The server SHALL publish conversation_handle in built-in MCP tool schemas; Actions schemas omit
   it. Keep Rust/Node catalog and toolset revisions synchronized.
FR-9: The server SHALL preserve existing workspace allowlists, permission checks, history session_key meanings,
   path restrictions and authenticated request verification.

## 非功能需求
Current OAuth tokens have no stable subject. Bind to a fingerprint of the already
verified bearer credential, scoped by workspace profile/runtime; refreshed or
rotated credentials require a new handle. Client names, IPs and supplied metadata
are not authentication. Handle secrecy remains important even with owner binding.

## 验证
Rust and Node store/HTTP tests: sequential reuse, independent conversations,
folder choices followed by binding, cwd restoration, native compatibility and
conflicts, different valid credentials, invalid auth, spoofed internal identities,
expiry, restart/capacity, invalid argument types, explicit route conflict rejection,
handle stripping and telemetry (including disabled general redaction). Include
process-control continuation using the handle and schema/catalog parity checks.

## 依赖关系
Existing verified HTTP authentication, workspace routing, tool catalog export, telemetry and secure random generation.
