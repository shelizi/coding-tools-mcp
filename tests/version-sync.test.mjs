import test from 'node:test';
import assert from 'node:assert/strict';

import {
  assertVersion,
  syncCargoLock,
  syncCargoManifest,
  syncPackageLock
} from '../scripts/sync-version.mjs';

test('validates semantic release versions', () => {
  assert.equal(assertVersion('1.2.3'), '1.2.3');
  assert.equal(assertVersion('1.2.3-rc.1+build.5'), '1.2.3-rc.1+build.5');
  assert.throws(() => assertVersion('1.2'), /invalid semantic version/);
});

test('synchronizes both root versions in package-lock.json', () => {
  const input = JSON.stringify(
    {
      name: 'coding-tools-mcp-desktop',
      version: '0.1.31',
      packages: { '': { name: 'coding-tools-mcp-desktop', version: '0.1.31' } }
    },
    null,
    2
  );
  const output = syncPackageLock(`${input}\n`, '0.1.32');
  const lock = JSON.parse(output);
  assert.equal(lock.version, '0.1.32');
  assert.equal(lock.packages[''].version, '0.1.32');
});

test('synchronizes the Rust manifest and lock package versions', () => {
  const manifest = '[package]\nname = "coding-tools-mcp-desktop"\nversion = "0.1.31"\nedition = "2021"\n';
  const lock = '[[package]]\nname = "coding-tools-mcp-desktop"\nversion = "0.1.31"\ndependencies = []\n';

  assert.match(syncCargoManifest(manifest, '0.1.32'), /version = "0\.1\.32"/);
  assert.match(syncCargoLock(lock, '0.1.32'), /version = "0\.1\.32"/);
});
