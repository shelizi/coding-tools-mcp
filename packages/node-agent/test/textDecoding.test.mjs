import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';
import { decodeTextBuffer, readDecodedTextFile } from '../dist/textCodec.js';

function config(root, dataDir) {
  return {
    host: '127.0.0.1',
    port: 0,
    dataDir,
    permissionMode: 'trusted',
    management: { enabled: false },
    oauth: {
      clientId: 'chatgpt',
      password: 'text-decoding-password',
      tokenSecret: 'text-decoding-token-secret'
    },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: {
      blockingConcurrency: 4,
      processConcurrency: 4,
      activeSessionLimit: 16,
      maxOutputBytes: 1024 * 1024
    }
  };
}

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-text-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-text-data-'));
  t.after(async () => {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const ctx = await createToolContext(config(root, dataDir));
  const meta = { 'openai/session': `text-${Date.now()}-${Math.random()}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  return { root, ctx, meta };
}

function encodeUtf8(text, bom = false) {
  const body = Buffer.from(text, 'utf8');
  return bom ? Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), body]) : body;
}

function encodeUtf16(text, endian) {
  const little = Buffer.from(text, 'utf16le');
  if (endian === 'le') return Buffer.concat([Buffer.from([0xff, 0xfe]), little]);
  const big = Buffer.allocUnsafe(little.length);
  for (let index = 0; index < little.length; index += 2) {
    big[index] = little[index + 1];
    big[index + 1] = little[index];
  }
  return Buffer.concat([Buffer.from([0xfe, 0xff]), big]);
}

async function writeFixtures(root, entries) {
  for (const [name, bytes] of Object.entries(entries)) await writeFile(path.join(root, name), bytes);
}

async function readTool(ctx, meta, file, extra = {}) {
  return callTool(ctx, 'read_file', { path: file, ...extra }, meta);
}

test('read_file and read_many report UTF-8 and BOM-marked UTF-16 encodings', async t => {
  const { root, ctx, meta } = await fixture(t);
  const text = 'alpha\nβeta\nemoji 😀\n';
  const fixtures = {
    'utf8.txt': encodeUtf8(text),
    'utf8-bom.txt': encodeUtf8(text, true),
    'utf16le.txt': encodeUtf16(text, 'le'),
    'utf16be.txt': encodeUtf16(text, 'be')
  };
  await writeFixtures(root, fixtures);

  const expected = [
    ['utf8.txt', 'utf-8', false],
    ['utf8-bom.txt', 'utf-8', true],
    ['utf16le.txt', 'utf-16le', true],
    ['utf16be.txt', 'utf-16be', true]
  ];
  for (const [file, encoding, bom] of expected) {
    const result = await readTool(ctx, meta, file);
    assert.equal(result.ok, true, JSON.stringify(result));
    assert.equal(result.content, text);
    assert.equal(result.encoding, encoding);
    assert.equal(result.bom, bom);
    assert.equal(result.total_bytes, fixtures[file].length);
    assert.equal(result.bytes_read, Buffer.byteLength(text));
    assert.equal(result.sha256, createHash('sha256').update(fixtures[file]).digest('hex'));
  }

  const many = await callTool(ctx, 'read_many', {
    items: expected.map(([file]) => ({ path: file })),
    line_numbers: true
  }, meta);
  assert.equal(many.ok, true, JSON.stringify(many));
  assert.equal(many.failed_count, 0);
  assert.deepEqual(many.results.map(result => result.encoding), expected.map(([, encoding]) => encoding));
  assert.ok(many.results.every(result => result.numbered_content.includes('1 | alpha')));
});

test('read_file truncates decoded Unicode on UTF-8 byte boundaries', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'unicode.txt'), encodeUtf16('A😀B\n', 'le'));
  const result = await readTool(ctx, meta, 'unicode.txt', { max_bytes: 4 });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.content, 'A');
  assert.equal(result.bytes_read, 1);
  assert.equal(result.truncated, true);
  assert.equal(result.truncated_by, 'bytes');
});

test('search_text and project_map consume the shared BOM-aware decoder', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'utf16le.txt'), encodeUtf16('first\nneedle β\n', 'le'));
  await writeFile(path.join(root, 'utf16be.txt'), encodeUtf16('other\nneedle γ\n', 'be'));
  await writeFile(path.join(root, 'package.json'), encodeUtf16(JSON.stringify({ scripts: { test: 'node --test', build: 'tsc' } }), 'le'));

  const searched = await callTool(ctx, 'search_text', {
    query: 'needle',
    include_globs: ['utf16*.txt']
  }, meta);
  assert.equal(searched.ok, true, JSON.stringify(searched));
  assert.equal(searched.total_matches, 2);
  assert.deepEqual(searched.matches.map(match => match.path), ['utf16be.txt', 'utf16le.txt']);

  const project = await callTool(ctx, 'project_map', { max_depth: 2 }, meta);
  assert.equal(project.ok, true, JSON.stringify(project));
  assert.equal(project.package_scripts.test, 'node --test');
  assert.ok(project.suggested_commands.some(item => item.command === 'npm run test'));
});

test('edit_file and edit_many preserve UTF-16 byte order and BOM', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'little.txt'), encodeUtf16('value = 1\n', 'le'));
  await writeFile(path.join(root, 'big.txt'), encodeUtf16('value = 1\n', 'be'));
  await writeFile(path.join(root, 'utf8-bom.txt'), encodeUtf8('value = 1\n', true));

  const little = await callTool(ctx, 'edit_file', {
    path: 'little.txt',
    edits: [{ type: 'replace', old_text: 'value = 1', new_text: 'value = 2' }]
  }, meta);
  assert.equal(little.ok, true, JSON.stringify(little));
  assert.equal(little.encoding, 'utf-16le');
  assert.equal(little.bom, true);
  const littleBytes = await readFile(path.join(root, 'little.txt'));
  assert.deepEqual([...littleBytes.subarray(0, 2)], [0xff, 0xfe]);
  assert.equal(decodeTextBuffer(littleBytes).text, 'value = 2\n');
  assert.equal(little.after_sha256, createHash('sha256').update(littleBytes).digest('hex'));

  const utf8Bom = await callTool(ctx, 'edit_file', {
    path: 'utf8-bom.txt',
    edits: [{ type: 'replace', old_text: 'value = 1', new_text: 'value = 5' }]
  }, meta);
  assert.equal(utf8Bom.ok, true, JSON.stringify(utf8Bom));
  assert.equal(utf8Bom.encoding, 'utf-8');
  assert.equal(utf8Bom.bom, true);
  const utf8BomBytes = await readFile(path.join(root, 'utf8-bom.txt'));
  assert.deepEqual([...utf8BomBytes.subarray(0, 3)], [0xef, 0xbb, 0xbf]);
  assert.equal(decodeTextBuffer(utf8BomBytes).text, 'value = 5\n');

  const many = await callTool(ctx, 'edit_many', {
    files: [
      { path: 'little.txt', edits: [{ type: 'replace', old_text: 'value = 2', new_text: 'value = 3' }] },
      { path: 'big.txt', edits: [{ type: 'replace', old_text: 'value = 1', new_text: 'value = 4' }] }
    ]
  }, meta);
  assert.equal(many.ok, true, JSON.stringify(many));
  assert.deepEqual(many.results.map(result => result.encoding), ['utf-16le', 'utf-16be']);
  const bigBytes = await readFile(path.join(root, 'big.txt'));
  assert.deepEqual([...bigBytes.subarray(0, 2)], [0xfe, 0xff]);
  assert.equal(decodeTextBuffer(bigBytes).text, 'value = 4\n');
});

test('malformed encodings and binary data return stable errors', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFixtures(root, {
    'invalid-utf8.txt': Buffer.from([0xc3, 0x28]),
    'odd-utf16.txt': Buffer.from([0xff, 0xfe, 0x41]),
    'surrogate-utf16.txt': Buffer.from([0xff, 0xfe, 0x00, 0xd8]),
    'binary.dat': Buffer.from([0x01, 0x00, 0x02, 0x03])
  });

  for (const file of ['invalid-utf8.txt', 'odd-utf16.txt', 'surrogate-utf16.txt']) {
    const result = await readTool(ctx, meta, file);
    assert.equal(result.ok, false);
    assert.equal(result.error.code, 'UNSUPPORTED_ENCODING');
    assert.equal(result.error.category, 'validation');
    assert.equal(result.error.retryable, false);
  }
  const binary = await readTool(ctx, meta, 'binary.dat');
  assert.equal(binary.ok, false);
  assert.equal(binary.error.code, 'BINARY_FILE');

  const editInvalid = await callTool(ctx, 'edit_file', {
    path: 'invalid-utf8.txt',
    edits: [{ type: 'replace', old_text: 'x', new_text: 'y' }]
  }, meta);
  assert.equal(editInvalid.ok, false);
  assert.equal(editInvalid.error.code, 'UNSUPPORTED_ENCODING');
  assert.deepEqual(await readFile(path.join(root, 'invalid-utf8.txt')), Buffer.from([0xc3, 0x28]));

  const many = await callTool(ctx, 'read_many', {
    items: [{ path: 'invalid-utf8.txt' }, { path: 'binary.dat' }]
  }, meta);
  assert.equal(many.ok, true);
  assert.equal(many.failed_count, 2);
  assert.deepEqual(many.results.map(result => result.error.code), ['UNSUPPORTED_ENCODING', 'BINARY_FILE']);
});

test('decoder byte limits are checked before text conversion', async t => {
  const { root } = await fixture(t);
  const file = path.join(root, 'bounded.txt');
  await writeFile(file, Buffer.alloc(32, 0x61));
  await assert.rejects(
    readDecodedTextFile(file, 8),
    error => error?.code === 'FILE_TOO_LARGE'
      && error?.category === 'limit'
      && error?.retryable === true
      && error?.details?.total_bytes === 32
      && error?.details?.max_bytes === 8
  );
});

test('Git patch preflight rejects UTF-16 instead of applying with byte-unsafe semantics', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'patch.txt'), encodeUtf16('old\n', 'le'));
  const result = await callTool(ctx, 'patch_check', {
    patch: [
      '--- a/patch.txt',
      '+++ b/patch.txt',
      '@@ -1,1 +1,1 @@',
      '-old',
      '+new',
      ''
    ].join('\n')
  }, meta);
  assert.equal(result.ok, false);
  assert.equal(result.error.code, 'UNSUPPORTED_ENCODING');
  assert.equal(result.error.details.encoding, 'utf-16le');
  assert.equal(decodeTextBuffer(await readFile(path.join(root, 'patch.txt'))).text, 'old\n');
});
