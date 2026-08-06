import test from 'node:test';
import assert from 'node:assert/strict';
import { EventEmitter } from 'node:events';
import { PassThrough } from 'node:stream';
import { spawn } from 'node:child_process';
import {
  ProcessStartupController,
  ProcessStartupError,
  WINDOWS_CIRCUIT_BREAKER_DELAY_MS,
  WINDOWS_CIRCUIT_BREAKER_THRESHOLD,
  WINDOWS_DLL_INIT_FAILED_SIGNED,
  WINDOWS_DLL_INIT_FAILED_UNSIGNED,
  WINDOWS_FAILURE_WINDOW_MS,
  WINDOWS_RETRY_DELAYS_MS,
  WINDOWS_START_INTERVAL_MS,
  WINDOWS_STARTUP_PROBE_MS,
  WINDOWS_STARTUP_SLOTS,
  isWindowsLoaderInitializationFailure,
  startupDiagnosticsJson
} from '../dist/processStartup.js';
import { runBuffered } from '../dist/processes.js';

class FakeClock {
  nowMs = 0;
  sleeps = [];

  now() {
    return this.nowMs;
  }

  async sleep(ms, signal) {
    if (signal?.aborted) throw Object.assign(new Error('aborted'), { name: 'AbortError' });
    this.sleeps.push(ms);
    this.nowMs += ms;
    await Promise.resolve();
  }

  advance(ms) {
    this.nowMs += ms;
  }
}

class ManualClock extends FakeClock {
  pending = [];

  sleep(ms, signal) {
    if (signal?.aborted) return Promise.reject(Object.assign(new Error('aborted'), { name: 'AbortError' }));
    this.sleeps.push(ms);
    return new Promise((resolve, reject) => {
      const wait = { ms, resolve, reject, signal, onAbort: undefined };
      wait.onAbort = () => {
        const index = this.pending.indexOf(wait);
        if (index >= 0) this.pending.splice(index, 1);
        signal?.removeEventListener('abort', wait.onAbort);
        reject(Object.assign(new Error('aborted'), { name: 'AbortError' }));
      };
      signal?.addEventListener('abort', wait.onAbort, { once: true });
      this.pending.push(wait);
    });
  }

  releaseNext() {
    const wait = this.pending.shift();
    assert.ok(wait, 'expected a pending clock wait');
    wait.signal?.removeEventListener('abort', wait.onAbort);
    this.nowMs += wait.ms;
    wait.resolve();
  }
}

let nextPid = 10_000;

class FakeChild extends EventEmitter {
  stdin = new PassThrough();
  stdout = new PassThrough();
  stderr = new PassThrough();
  pid = nextPid++;
  exitCode = null;
  signalCode = null;
  killed = false;

  kill(signal = 'SIGTERM') {
    this.killed = true;
    this.signalCode = signal;
    return true;
  }
}

function childWithExit(exitCode = null) {
  const child = new FakeChild();
  child.exitCode = exitCode;
  queueMicrotask(() => child.emit('spawn'));
  return child;
}

function childWithSpawnError(message = 'spawn failed') {
  const child = new FakeChild();
  queueMicrotask(() => child.emit('error', new Error(message)));
  return child;
}

async function rejected(promise, kind) {
  try {
    await promise;
    assert.fail(`expected ${kind} startup failure`);
  } catch (error) {
    assert.ok(error instanceof ProcessStartupError, String(error));
    assert.equal(error.kind, kind);
    return error;
  }
}

test('Windows startup constants and signed loader status match Rust', () => {
  assert.equal(WINDOWS_STARTUP_SLOTS, 4);
  assert.equal(WINDOWS_START_INTERVAL_MS, 75);
  assert.equal(WINDOWS_STARTUP_PROBE_MS, 125);
  assert.equal(WINDOWS_FAILURE_WINDOW_MS, 10_000);
  assert.equal(WINDOWS_CIRCUIT_BREAKER_THRESHOLD, 3);
  assert.equal(WINDOWS_CIRCUIT_BREAKER_DELAY_MS, 3_000);
  assert.deepEqual([...WINDOWS_RETRY_DELAYS_MS], [250, 750, 1_500]);
  assert.equal(WINDOWS_DLL_INIT_FAILED_SIGNED, -1073741502);
  assert.equal(isWindowsLoaderInitializationFailure(WINDOWS_DLL_INIT_FAILED_SIGNED), true);
  assert.equal(isWindowsLoaderInitializationFailure(WINDOWS_DLL_INIT_FAILED_UNSIGNED), true);
  assert.equal(isWindowsLoaderInitializationFailure(1), false);
  assert.equal(isWindowsLoaderInitializationFailure(null), false);
});

test('non-Windows startup is a single attempt without gate or probe', async () => {
  const clock = new FakeClock();
  const controller = new ProcessStartupController({ platform: 'linux', clock });
  let launches = 0;
  const started = await controller.start(() => {
    launches += 1;
    return childWithExit(null);
  });
  assert.equal(launches, 1);
  assert.equal(started.earlyExit, false);
  assert.deepEqual(startupDiagnosticsJson(started.diagnostics), {
    attempts: 1,
    retry_count: 0,
    gate_wait_ms: 0,
    retry_delays_ms: [],
    error_dialog_suppressed: false,
    startup_slots: 0,
    start_interval_ms: 0
  });
  assert.deepEqual(clock.sleeps, []);
});

test('Windows retries loader failures with deterministic delays and circuit cooldown', async () => {
  const clock = new FakeClock();
  const controller = new ProcessStartupController({ platform: 'win32', clock });
  const outcomes = [
    WINDOWS_DLL_INIT_FAILED_SIGNED,
    WINDOWS_DLL_INIT_FAILED_UNSIGNED,
    WINDOWS_DLL_INIT_FAILED_SIGNED,
    null
  ];
  const started = await controller.start(() => childWithExit(outcomes.shift()));
  assert.equal(outcomes.length, 0);
  assert.equal(started.earlyExit, false);
  assert.deepEqual(started.diagnostics.retryDelaysMs, [250, 767, 1_534]);
  assert.equal(started.diagnostics.attempts, 4);
  assert.equal(started.diagnostics.startupSlots, 4);
  assert.equal(started.diagnostics.startIntervalMs, 75);
  assert.equal(started.diagnostics.errorDialogSuppressed, false);
  assert.equal(started.diagnostics.gateWaitMs, 1_466);
  assert.deepEqual(clock.sleeps, [125, 250, 125, 767, 125, 1_534, 1_466, 125]);
});

test('ordinary early exits and spawn errors remain single-attempt', async () => {
  const ordinaryClock = new FakeClock();
  const ordinary = await new ProcessStartupController({ platform: 'win32', clock: ordinaryClock })
    .start(() => childWithExit(1));
  assert.equal(ordinary.earlyExit, true);
  assert.equal(ordinary.child.exitCode, 1);
  assert.equal(ordinary.diagnostics.attempts, 1);
  assert.deepEqual(ordinary.diagnostics.retryDelaysMs, []);

  const errorClock = new FakeClock();
  let launches = 0;
  const failure = await rejected(new ProcessStartupController({ platform: 'win32', clock: errorClock }).start(() => {
    launches += 1;
    return childWithSpawnError('missing executable');
  }), 'spawn');
  assert.equal(launches, 1);
  assert.equal(failure.message, 'missing executable');
  assert.equal(failure.diagnostics.attempts, 1);
  assert.deepEqual(failure.diagnostics.retryDelaysMs, []);
});

test('startup slot and minimum launch spacing serialize only the probe phase', async () => {
  const clock = new ManualClock();
  const controller = new ProcessStartupController({
    platform: 'win32',
    clock,
    startupSlots: 1,
    probeMs: 125
  });
  let launches = 0;
  const first = controller.start(() => {
    launches += 1;
    return childWithExit(null);
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(launches, 1);
  assert.equal(clock.pending.length, 1);

  const second = controller.start(() => {
    launches += 1;
    return childWithExit(null);
  });
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(launches, 1, 'second launch waits for the startup slot');

  clock.releaseNext();
  await first;
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(launches, 2);
  assert.equal(clock.pending.length, 1);
  clock.releaseNext();
  await second;

  const spacingClock = new FakeClock();
  const spacing = new ProcessStartupController({ platform: 'win32', clock: spacingClock, probeMs: 0 });
  await spacing.start(() => childWithExit(null));
  const spaced = await spacing.start(() => childWithExit(null));
  assert.equal(spaced.diagnostics.gateWaitMs, 75);
  assert.deepEqual(spacingClock.sleeps, [75]);
});

test('circuit breaker delays a new launch and old failures expire', async () => {
  const clock = new FakeClock();
  const controller = new ProcessStartupController({ platform: 'win32', clock });
  const exhausted = await rejected(controller.start(() => childWithExit(WINDOWS_DLL_INIT_FAILED_SIGNED)), 'loader_initialization');
  assert.equal(exhausted.diagnostics.attempts, 4);
  assert.deepEqual(exhausted.diagnostics.retryDelaysMs, [250, 767, 1_534]);

  const afterCircuit = await controller.start(() => childWithExit(null));
  assert.equal(afterCircuit.diagnostics.gateWaitMs, 3_000);

  clock.advance(WINDOWS_FAILURE_WINDOW_MS + 1);
  const afterWindow = await controller.start(() => childWithExit(null));
  assert.equal(afterWindow.diagnostics.gateWaitMs, 0);
});

test('startup cancellation and total deadline stop retries without leaking the child', async () => {
  const cancelledController = new ProcessStartupController({ platform: 'win32', clock: new FakeClock() });
  const abort = new AbortController();
  abort.abort();
  let launches = 0;
  const cancelled = await rejected(cancelledController.start(() => {
    launches += 1;
    return childWithExit(null);
  }, { signal: abort.signal }), 'cancelled');
  assert.equal(launches, 0);
  assert.equal(cancelled.diagnostics.attempts, 0);

  const deadlineClock = new FakeClock();
  const deadlineController = new ProcessStartupController({ platform: 'win32', clock: deadlineClock });
  const timed = await rejected(deadlineController.start(
    () => childWithExit(WINDOWS_DLL_INIT_FAILED_SIGNED),
    { deadlineMs: 300 }
  ), 'timeout');
  assert.equal(timed.diagnostics.attempts, 1);
  assert.deepEqual(timed.diagnostics.retryDelaysMs, [250]);
  assert.deepEqual(deadlineClock.sleeps, [125, 175]);
});

test('real Windows child survives startup gate and probe with output intact', {
  skip: process.platform !== 'win32'
}, async () => {
  const controller = new ProcessStartupController();
  const started = await controller.start(() => spawn(process.execPath, [
    '-e',
    'setTimeout(() => { process.stdout.write("windows-startup-ok"); process.exit(0); }, 250)'
  ], {
    windowsHide: true,
    stdio: 'pipe'
  }));

  assert.equal(started.earlyExit, false);
  assert.equal(started.diagnostics.attempts, 1);
  assert.equal(started.diagnostics.startupSlots, WINDOWS_STARTUP_SLOTS);
  assert.equal(started.diagnostics.startIntervalMs, WINDOWS_START_INTERVAL_MS);
  assert.equal(started.diagnostics.errorDialogSuppressed, false);

  let stdout = '';
  started.child.stdout.on('data', chunk => { stdout += chunk.toString(); });
  const startupErrors = started.handoff();
  assert.deepEqual(startupErrors, []);
  const exitCode = await new Promise((resolve, reject) => {
    started.child.once('error', reject);
    started.child.once('close', resolve);
  });
  assert.equal(exitCode, 0);
  assert.equal(stdout, 'windows-startup-ok');
});

test('buffered startup preserves quick output and maps spawn failures', async () => {
  const quick = await runBuffered(
    process.execPath,
    ['-e', 'process.stdout.write("buffered-startup-ok")'],
    process.cwd(),
    undefined,
    10_000
  );
  assert.equal(quick.code, 0);
  assert.equal(quick.stdout, 'buffered-startup-ok');
  assert.equal(quick.stderr, '');

  const missing = `ctmcp-definitely-missing-executable-${process.pid}`;
  await assert.rejects(
    runBuffered(missing, [], process.cwd(), undefined, 1_000),
    error => {
      assert.equal(error.code, 'COMMAND_SPAWN_FAILED');
      assert.equal(error.retryable, true);
      assert.equal(error.details.termination_reason, 'spawn_failed');
      assert.equal(error.details.startup.attempts, 1);
      assert.equal(error.details.startup.retry_count, 0);
      return true;
    }
  );
});
