import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const testPath = fileURLToPath(import.meta.url);
const packageRoot = path.resolve(path.dirname(testPath), '..');
const scriptPath = path.join(packageRoot, 'scripts', 'with-dist-lock.mjs');

function distLockPath(lockRoot) {
  const normalizedPackageRoot = process.platform === 'win32' ? packageRoot.toLowerCase() : packageRoot;
  const lockKey = createHash('sha256').update(normalizedPackageRoot).digest('hex').slice(0, 24);
  return path.join(lockRoot, `${lockKey}.lock`);
}

test('with-dist-lock executes a native package manager npm_execpath directly', () => {
  const lockRoot = mkdtempSync(path.join(tmpdir(), 'ctmcp-dist-lock-native-pm-'));
  try {
    const result = spawnSync(
      process.execPath,
      [scriptPath, 'pnpm', '-e', 'process.stdout.write("native-package-manager-ok")'],
      {
        cwd: packageRoot,
        encoding: 'utf8',
        env: {
          ...process.env,
          npm_execpath: process.execPath,
          CTMCP_DIST_LOCK_ROOT: lockRoot
        }
      }
    );

    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, 'native-package-manager-ok');
  } finally {
    rmSync(lockRoot, { recursive: true, force: true });
  }
});

test('with-dist-lock keeps JavaScript package manager npm_execpath on Node', () => {
  const lockRoot = mkdtempSync(path.join(tmpdir(), 'ctmcp-dist-lock-js-pm-'));
  const fakePackageManager = path.join(lockRoot, 'fake-pnpm.cjs');
  try {
    writeFileSync(fakePackageManager, 'process.stdout.write(process.argv.slice(2).join("|"));\n', 'utf8');
    const result = spawnSync(
      process.execPath,
      [scriptPath, 'pnpm', 'alpha', 'beta'],
      {
        cwd: packageRoot,
        encoding: 'utf8',
        env: {
          ...process.env,
          npm_execpath: fakePackageManager,
          CTMCP_DIST_LOCK_ROOT: lockRoot
        }
      }
    );

    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, 'alpha|beta');
  } finally {
    rmSync(lockRoot, { recursive: true, force: true });
  }
});

test('with-dist-lock immediately recovers a fresh lock whose owner process is gone', () => {
  const lockRoot = mkdtempSync(path.join(tmpdir(), 'ctmcp-dist-lock-dead-owner-'));
  try {
    writeFileSync(
      distLockPath(lockRoot),
      `${JSON.stringify({ pid: 2147483647, token: 'dead-owner', startedAt: new Date().toISOString() })}\n`,
      'utf8'
    );

    const started = Date.now();
    const result = spawnSync(
      process.execPath,
      [scriptPath, process.execPath, '-e', 'process.stdout.write("dead-owner-recovered")'],
      {
        cwd: packageRoot,
        encoding: 'utf8',
        env: {
          ...process.env,
          CTMCP_DIST_LOCK_ROOT: lockRoot,
          CTMCP_DIST_LOCK_TIMEOUT_MS: '1000',
          CTMCP_DIST_LOCK_STALE_MS: '600000'
        }
      }
    );

    assert.equal(result.status, 0, result.stderr);
    assert.equal(result.stdout, 'dead-owner-recovered');
    assert.ok(Date.now() - started < 1000, `dead owner recovery took too long: ${Date.now() - started}ms`);
  } finally {
    rmSync(lockRoot, { recursive: true, force: true });
  }
});
