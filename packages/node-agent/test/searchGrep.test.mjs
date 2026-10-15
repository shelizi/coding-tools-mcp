import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { resetSearchBackendCache, resolveGrepProgram, runExternalContentSearch } from '../dist/searchGrep.js';

test('GNU grep searches the workspace root with shared ignore and path filters', {
  skip: !resolveGrepProgram()
}, async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-grep-'));
  const previous = process.env.CTMCP_SEARCH_BACKEND;
  process.env.CTMCP_SEARCH_BACKEND = 'grep';
  resetSearchBackendCache();
  t.after(async () => {
    if (previous === undefined) delete process.env.CTMCP_SEARCH_BACKEND;
    else process.env.CTMCP_SEARCH_BACKEND = previous;
    resetSearchBackendCache();
    await rm(root, { recursive: true, force: true, maxRetries: 5 });
  });
  async function put(relative, content = 'needle\n') {
    const file = path.join(root, relative);
    await mkdir(path.dirname(file), { recursive: true });
    await writeFile(file, content);
  }
  await put('.gitignore', '*.log\n!keep.log\n');
  await put('.ignore', 'ignored.txt\n');
  await put('.git/info/exclude', 'info.txt\n');
  for (const file of ['visible.txt', 'UPPER.TXT', 'drop.log', 'keep.log', 'ignored.txt', 'info.txt',
    '.hidden/file.txt', 'src/keep.txt', 'other/keep.txt', 'src/[literal].txt', 'excluded/file.txt']) {
    await put(file);
  }
  async function search(overrides = {}) {
    const hits = [];
    const stats = await runExternalContentSearch({
      root, searchPath: '.', isDirectory: true,
      queries: [{ query: 'needle', regex: false, caseSensitive: true }],
      includeGlobs: [], excludeGlobs: [], excludedPrefixes: [],
      includeHidden: false, includeIgnored: false, includeGenerated: false,
      restrictToText: true, contextLines: 0, maxFileBytes: 1024 * 1024,
      maxMatchesPerFile: 10, ...overrides
    }, hit => { hits.push(hit.path); return true; });
    assert.equal(stats?.backend, 'grep');
    return hits.sort();
  }
  assert.deepEqual(await search(), ['UPPER.TXT', 'excluded/file.txt', 'keep.log', 'other/keep.txt',
    'src/[literal].txt', 'src/keep.txt', 'visible.txt'].sort());
  assert.deepEqual(await search({ includeGlobs: ['src/*.txt'], excludeGlobs: ['src/keep.txt'] }), ['src/[literal].txt']);
  assert.deepEqual(await search({ includeGlobs: ['**/keep.txt'], excludeGlobs: ['src/keep.txt'] }), ['other/keep.txt']);
  assert.deepEqual(await search({ includeGlobs: ['src/[literal].txt'] }), ['src/[literal].txt']);
  assert(!(await search({ excludedPrefixes: ['excluded'] })).includes('excluded/file.txt'));
  assert((await search({ includeHidden: true })).includes('.hidden/file.txt'));
  const all = await search({ includeIgnored: true });
  for (const file of ['drop.log', 'ignored.txt', 'info.txt']) assert(all.includes(file), file);
  assert.deepEqual(await search({ searchPath: 'ignored.txt', isDirectory: false }), []);
  assert.deepEqual(await search({ searchPath: 'visible.txt', isDirectory: false, excludeGlobs: ['visible.txt'] }), []);
});
