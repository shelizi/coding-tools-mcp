import test from 'node:test';
import assert from 'node:assert/strict';

import { runCapturedStdoutWithProgress } from '../../../scripts/run-streamed-child.mjs';

function sink() {
  const writes = [];
  return {
    writes,
    stream: {
      write(chunk) {
        writes.push({ at: Date.now(), text: String(chunk) });
        return true;
      }
    }
  };
}

test('streamed child keeps stdout clean while forwarding stderr progress', async () => {
  const captured = sink();
  const stdout = await runCapturedStdoutWithProgress(
    process.execPath,
    ['-e', "process.stderr.write('compile progress\\n'); process.stdout.write('{\\\"ok\\\":true}');"],
    { label: 'catalog-test', quietHeartbeatMs: 0, stderr: captured.stream }
  );

  assert.equal(stdout, '{"ok":true}');
  assert.match(captured.writes.map(item => item.text).join(''), /compile progress/);
});

test('streamed child emits a quiet progress heartbeat before completion', async () => {
  const captured = sink();
  const startedAt = Date.now();
  const stdout = await runCapturedStdoutWithProgress(
    process.execPath,
    ['-e', "setTimeout(() => process.stdout.write('done'), 120);"],
    { label: 'catalog-test', quietHeartbeatMs: 30, stderr: captured.stream }
  );
  const completedAt = Date.now();

  assert.equal(stdout, 'done');
  const heartbeat = captured.writes.find(item => item.text.includes('still running'));
  assert.ok(heartbeat, 'expected a quiet-period progress heartbeat');
  assert.ok(heartbeat.at >= startedAt && heartbeat.at < completedAt);
  assert.match(heartbeat.text, /catalog-test/);
});

test('streamed child can suppress noisy stderr while retaining captured stdout', async () => {
  const captured = sink();
  const stdout = await runCapturedStdoutWithProgress(
    process.execPath,
    ['-e', "process.stderr.write('warning noise\\n'); process.stdout.write('ok');"],
    { label: 'catalog-test', quietHeartbeatMs: 0, stderr: captured.stream, streamStderr: false }
  );

  assert.equal(stdout, 'ok');
  assert.equal(captured.writes.length, 0);
});

test('hidden stderr activity does not suppress user-visible quiet heartbeats', async () => {
  const captured = sink();
  const stdout = await runCapturedStdoutWithProgress(
    process.execPath,
    ['-e', "const timer=setInterval(() => process.stderr.write('hidden warning\\n'), 10); setTimeout(() => { clearInterval(timer); process.stdout.write('done'); }, 360);"],
    { label: 'catalog-test', quietHeartbeatMs: 80, stderr: captured.stream, streamStderr: false }
  );

  assert.equal(stdout, 'done');
  assert.ok(captured.writes.filter(item => item.text.includes('still running')).length >= 2);
  assert.doesNotMatch(captured.writes.map(item => item.text).join(''), /hidden warning/);
});
test('streamed child keeps suppressed stderr available for failures', async () => {
  const captured = sink();
  await assert.rejects(
    runCapturedStdoutWithProgress(
      process.execPath,
      ['-e', "process.stderr.write('link failed\\n'); process.exit(7);"],
      { label: 'catalog-test', quietHeartbeatMs: 0, stderr: captured.stream, streamStderr: false }
    ),
    /link failed/
  );
  assert.equal(captured.writes.length, 0);
});