import test from 'node:test';
import assert from 'node:assert/strict';
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { listSearchSoftware, readDownloadConfig, uninstallRipgrep, writeDownloadConfig } from '../dist/searchSoftware.js';
import { managedRipgrepPath, resetSearchBackendCache } from '../dist/searchGrep.js';

async function fixture(t) {
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-search-software-'));
  t.after(async () => {
    resetSearchBackendCache();
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  return dataDir;
}

test('lists ripgrep and grep as Node Agent search tools', async t => {
  const dataDir = await fixture(t);
  resetSearchBackendCache();
  const items = listSearchSoftware(dataDir);
  assert.deepEqual(items.map(item => item.kind), ['rg', 'grep']);
  assert.equal(items[0].group, 'search');
  assert.equal(items[1].group, 'search');
  assert.equal(items[1].installable, false);
  assert.equal(items[1].managed, false);
  if (!items[0].installed) {
    assert.equal(items[0].installable, true);
    assert.match(items[0].hint, /Node\.js search/);
  }
});

test('detects a managed ripgrep binary in the agent data directory', async t => {
  const dataDir = await fixture(t);
  const managed = managedRipgrepPath(dataDir);
  await mkdir(path.dirname(managed), { recursive: true });
  if (process.platform === 'win32') {
    await copyFile(process.execPath, managed);
  } else {
    await writeFile(managed, '#!/bin/sh\necho ripgrep 14.1.1\nexit 0\n', { encoding: 'utf8' });
    await chmod(managed, 0o755);
  }
  resetSearchBackendCache();
  const items = listSearchSoftware(dataDir);
  assert.equal(items[0].installed, true);
  assert.equal(items[0].managed, true);
  assert.equal(items[0].path, managed);
  await uninstallRipgrep(dataDir);
  const after = listSearchSoftware(dataDir);
  assert.equal(after[0].managed, false);
});

test('persists download config for ripgrep GitHub installs', async t => {
  const dataDir = await fixture(t);
  const saved = await writeDownloadConfig(dataDir, {
    githubMirror: 'https://mirror.example',
    proxyMode: 'manual',
    proxyUrl: 'http://127.0.0.1:7890'
  });
  assert.equal(saved.githubMirror, 'https://mirror.example');
  const loaded = await readDownloadConfig(dataDir);
  assert.deepEqual(loaded, saved);
});
