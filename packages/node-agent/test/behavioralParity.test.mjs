import test from 'node:test';
import assert from 'node:assert/strict';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { normalizeConfig } from '../dist/config.js';
import { MAX_CONVERSATION_CONTEXTS } from '../dist/conversation.js';
import { ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, DEFAULT_COMMAND_TIMEOUT_MAX_MS } from '../dist/executionLimits.js';
import {
  WINDOWS_CIRCUIT_BREAKER_DELAY_MS,
  WINDOWS_CIRCUIT_BREAKER_THRESHOLD,
  WINDOWS_DLL_INIT_FAILED_SIGNED,
  WINDOWS_FAILURE_WINDOW_MS,
  WINDOWS_RETRY_DELAYS_MS,
  WINDOWS_START_INTERVAL_MS,
  WINDOWS_STARTUP_PROBE_MS,
  WINDOWS_STARTUP_SLOTS
} from '../dist/processStartup.js';
import { rustBehavioralParityFixtures } from '../dist/rustCatalog.generated.js';
import {
  BUILTIN_TUNNEL_DEMAND_TTL_MS,
  BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS
} from '../dist/tunnel.js';
import {
  LATEST_MCP_PROTOCOL_VERSION,
  MCP_STREAM_CHANNEL_CAPACITY,
  MCP_STREAM_HEARTBEAT_INTERVAL_MS,
  SUPPORTED_MCP_PROTOCOL_VERSIONS
} from '../dist/mcpTransport.js';

function record(value, name) {
  assert.ok(value && typeof value === 'object' && !Array.isArray(value), `${name} fixture is missing`);
  return value;
}

test('Rust behavioral parity fixtures match Node runtime constants', () => {
  const execution = record(rustBehavioralParityFixtures.execution_limits, 'execution_limits');
  const workspace = record(rustBehavioralParityFixtures.workspace, 'workspace');
  const startup = record(rustBehavioralParityFixtures.process_start, 'process_start');
  const mcpTransport = record(rustBehavioralParityFixtures.mcp_transport, 'mcp_transport');
  const tunnel = record(rustBehavioralParityFixtures.tunnel, 'tunnel');
  const config = normalizeConfig({}, {}, {
    CTMCP_DATA_DIR: path.join(tmpdir(), 'ctmcp-parity-config')
  });

  assert.deepEqual(config.limits, {
    blockingConcurrency: execution.blocking_admission,
    processConcurrency: execution.process_admission,
    globalBlockingConcurrency: execution.global_blocking_admission,
    globalProcessConcurrency: execution.global_process_admission,
    activeSessionLimit: execution.active_sessions,
    maxOutputBytes: 1024 * 1024,
    commandTimeoutMaxMs: execution.command_timeout_default_ms
  });
  assert.equal(MAX_CONVERSATION_CONTEXTS, workspace.max_conversation_contexts);
  assert.equal(DEFAULT_COMMAND_TIMEOUT_MAX_MS, execution.command_timeout_default_ms);
  assert.equal(ABSOLUTE_COMMAND_TIMEOUT_MAX_MS, execution.command_timeout_absolute_max_ms);
  assert.deepEqual({
    startup_slots: WINDOWS_STARTUP_SLOTS,
    start_interval_ms: WINDOWS_START_INTERVAL_MS,
    startup_probe_ms: WINDOWS_STARTUP_PROBE_MS,
    failure_window_ms: WINDOWS_FAILURE_WINDOW_MS,
    circuit_breaker_threshold: WINDOWS_CIRCUIT_BREAKER_THRESHOLD,
    circuit_breaker_delay_ms: WINDOWS_CIRCUIT_BREAKER_DELAY_MS,
    retry_delays_ms: [...WINDOWS_RETRY_DELAYS_MS],
    status_dll_init_failed: WINDOWS_DLL_INIT_FAILED_SIGNED
  }, startup);
  assert.deepEqual({
    latest_protocol_version: LATEST_MCP_PROTOCOL_VERSION,
    supported_protocol_versions: [...SUPPORTED_MCP_PROTOCOL_VERSIONS],
    stream_heartbeat_interval_ms: MCP_STREAM_HEARTBEAT_INTERVAL_MS,
    stream_channel_capacity: MCP_STREAM_CHANNEL_CAPACITY
  }, mcpTransport);
  assert.deepEqual({
    demand_hint_ttl_ms: BUILTIN_TUNNEL_DEMAND_TTL_MS,
    local_connect_timeout_ms: BUILTIN_TUNNEL_LOCAL_CONNECT_TIMEOUT_MS
  }, tunnel);
});
