import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { decode as decodeJpeg, encode as encodeJpeg } from 'jpeg-js';
import { PNG } from 'pngjs';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';
import { identifyImage } from '../dist/imageCodec.js';

function config(root, dataDir) {
  return {
    host: '127.0.0.1', port: 0, dataDir, permissionMode: 'trusted',
    management: { enabled: false },
    oauth: { clientId: 'chatgpt', password: 'image-test-password', tokenSecret: 'image-test-token-secret' },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: { blockingConcurrency: 4, processConcurrency: 4, activeSessionLimit: 16, maxOutputBytes: 16 * 1024 * 1024 }
  };
}

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-image-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-image-data-'));
  t.after(async () => {
    await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
    await rm(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
  });
  const ctx = await createToolContext(config(root, dataDir));
  const meta = { 'openai/session': `image-${Date.now()}-${Math.random()}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  return { root, ctx, meta };
}

function rgba(width, height, pixel) {
  const data = Buffer.alloc(width * height * 4);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      const offset = (y * width + x) * 4;
      const [red, green, blue, alpha = 255] = pixel(x, y);
      data[offset] = red;
      data[offset + 1] = green;
      data[offset + 2] = blue;
      data[offset + 3] = alpha;
    }
  }
  return data;
}

function png(width, height, pixel) {
  const image = new PNG({ width, height });
  image.data = rgba(width, height, pixel);
  return PNG.sync.write(image, { colorType: 6, inputColorType: 6, inputHasAlpha: true });
}

function jpeg(width, height, pixel, quality = 90) {
  return encodeJpeg({ width, height, data: rgba(width, height, pixel) }, quality).data;
}

function noisyPixel(x, y) {
  return [
    (x * 73 + y * 151 + 19) % 256,
    (x * 199 + y * 47 + 83) % 256,
    (x * 31 + y * 223 + 137) % 256,
    255
  ];
}

const WEBP_1X1 = Buffer.from('UklGRiIAAABXRUJQVlA4IBYAAAAwAQCdASoBAAEAAUAmJaQAA3AA/v89WAAAAA==', 'base64');
const GIF_1X1 = Buffer.from('R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==', 'base64');

function gifCanvas(width, height) {
  const data = Buffer.from(GIF_1X1);
  data.writeUInt16LE(width, 6);
  data.writeUInt16LE(height, 8);
  return data;
}

async function view(ctx, meta, pathValue, extra = {}) {
  return callTool(ctx, 'view_image', { path: pathValue, output: 'data_url', ...extra }, meta);
}

test('view_image identifies PNG, JPEG, WebP and GIF from content rather than extension', async t => {
  const { root, ctx, meta } = await fixture(t);
  const pngData = png(8, 4, (x, y) => [x * 20, y * 40, 180, 255]);
  const jpegData = jpeg(7, 3, (x, y) => [x * 30, 100, y * 60, 255]);
  const gifData = gifCanvas(2, 1);
  await writeFile(path.join(root, 'actually-png.jpg'), pngData);
  await writeFile(path.join(root, 'actually-jpeg.png'), jpegData);
  await writeFile(path.join(root, 'tiny.bin'), WEBP_1X1);
  await writeFile(path.join(root, 'canvas.dat'), gifData);

  const pngResult = await view(ctx, meta, 'actually-png.jpg');
  assert.equal(pngResult.ok, true, JSON.stringify(pngResult));
  assert.equal(pngResult.mime_type, 'image/png');
  assert.equal(pngResult.width, 8);
  assert.equal(pngResult.height, 4);
  assert.equal(pngResult.resized, false);
  assert.deepEqual(pngResult.original, { bytes: pngData.length, width: 8, height: 4, mime_type: 'image/png' });
  assert.ok(Buffer.from(pngResult.base64, 'base64').equals(pngData));
  assert.match(pngResult.data_url, /^data:image\/png;base64,/);
  assert.equal(pngResult.content, undefined);

  const jpegResult = await callTool(ctx, 'view_image', { path: 'actually-jpeg.png' }, meta);
  assert.equal(jpegResult.ok, true, JSON.stringify(jpegResult));
  assert.equal(jpegResult.mime_type, 'image/jpeg');
  assert.equal(jpegResult.width, 7);
  assert.equal(jpegResult.height, 3);
  assert.equal(jpegResult.content[0].type, 'image');
  assert.equal(jpegResult.content[0].mimeType, 'image/jpeg');
  assert.equal(jpegResult.content[0].data, jpegResult.base64);

  const webpResult = await view(ctx, meta, 'tiny.bin');
  assert.equal(webpResult.ok, true, JSON.stringify(webpResult));
  assert.deepEqual([webpResult.mime_type, webpResult.width, webpResult.height], ['image/webp', 1, 1]);

  const gifResult = await view(ctx, meta, 'canvas.dat', { auto_resize: false });
  assert.equal(gifResult.ok, true, JSON.stringify(gifResult));
  assert.deepEqual([gifResult.mime_type, gifResult.width, gifResult.height], ['image/gif', 2, 1]);
});

test('PNG resize is proportional, decodable and leaves the source file unchanged', async t => {
  const { root, ctx, meta } = await fixture(t);
  const source = png(40, 20, (x, y) => [x * 5, y * 10, (x + y) * 3, 255]);
  await writeFile(path.join(root, 'wide.png'), source);

  const result = await view(ctx, meta, 'wide.png', { max_width: 10, max_height: 10 });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.resized, true);
  assert.equal(result.mime_type, 'image/png');
  assert.deepEqual([result.width, result.height], [10, 5]);
  assert.deepEqual(result.original, { bytes: source.length, width: 40, height: 20, mime_type: 'image/png' });
  const decoded = PNG.sync.read(Buffer.from(result.base64, 'base64'));
  assert.deepEqual([decoded.width, decoded.height], [10, 5]);
  assert.ok((await readFile(path.join(root, 'wide.png'))).equals(source));
});

test('JPEG resize is proportional and remains decodable', async t => {
  const { root, ctx, meta } = await fixture(t);
  const source = jpeg(30, 10, (x, y) => [x * 7, 80 + y * 8, 160, 255]);
  await writeFile(path.join(root, 'wide.jpeg'), source);

  const result = await view(ctx, meta, 'wide.jpeg', { max_width: 10, max_height: 10 });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.resized, true);
  assert.equal(result.mime_type, 'image/jpeg');
  assert.deepEqual([result.width, result.height], [10, 3]);
  const decoded = decodeJpeg(Buffer.from(result.base64, 'base64'), { useTArray: false, formatAsRGBA: true });
  assert.deepEqual([decoded.width, decoded.height], [10, 3]);
  assert.ok((await readFile(path.join(root, 'wide.jpeg'))).equals(source));
});

test('oversized PNG falls back through bounded JPEG quality levels', async t => {
  const { root, ctx, meta } = await fixture(t);
  const source = png(128, 128, noisyPixel);
  assert.ok(source.length > 12_000);
  await writeFile(path.join(root, 'noise.png'), source);

  const result = await view(ctx, meta, 'noise.png', {
    max_bytes: 12_000,
    max_width: 128,
    max_height: 128
  });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.resized, true);
  assert.equal(result.mime_type, 'image/jpeg');
  assert.ok(result.bytes <= 12_000);
  assert.deepEqual([result.width, result.height], [128, 128]);
  const decoded = decodeJpeg(Buffer.from(result.base64, 'base64'), { useTArray: false, formatAsRGBA: true });
  assert.deepEqual([decoded.width, decoded.height], [128, 128]);
});

test('unsupported resize formats return warnings without corrupting valid output', async t => {
  const { root, ctx, meta } = await fixture(t);
  const gifData = gifCanvas(2, 1);
  await writeFile(path.join(root, 'animated.gif'), gifData);

  const result = await view(ctx, meta, 'animated.gif', { max_width: 1, max_height: 1 });
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.resized, false);
  assert.equal(result.mime_type, 'image/gif');
  assert.deepEqual([result.width, result.height], [2, 1]);
  assert.ok(result.warnings.some(warning => warning.includes('image/gif resize is unsupported')));
  assert.ok(Buffer.from(result.base64, 'base64').equals(gifData));
});

test('invalid images and disabled resizing return stable Rust error codes', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'fake.png'), 'not an image');
  const invalid = await view(ctx, meta, 'fake.png');
  assert.equal(invalid.ok, false);
  assert.equal(invalid.error.code, 'BINARY_FILE');
  assert.equal(invalid.error.category, 'validation');

  const source = png(128, 128, noisyPixel);
  await writeFile(path.join(root, 'large.png'), source);
  const oversized = await view(ctx, meta, 'large.png', { max_bytes: 1024, auto_resize: false });
  assert.equal(oversized.ok, false);
  assert.equal(oversized.error.code, 'OUTPUT_TOO_LARGE');
  assert.equal(oversized.error.category, 'validation');
  assert.equal(oversized.error.details.max_bytes, 1024);
  assert.equal(oversized.error.details.actual_bytes, source.length);

  const truncatedGif = Buffer.from('47494638396101000100800000', 'hex');
  assert.throws(() => identifyImage(truncatedGif), error => error?.code === 'BINARY_FILE');

  const webpHeaderOnly = Buffer.alloc(30);
  webpHeaderOnly.write('RIFF', 0, 'ascii');
  webpHeaderOnly.writeUInt32LE(22, 4);
  webpHeaderOnly.write('WEBPVP8X', 8, 'ascii');
  webpHeaderOnly.writeUInt32LE(10, 16);
  assert.throws(() => identifyImage(webpHeaderOnly), error => error?.code === 'BINARY_FILE');

  assert.deepEqual(identifyImage(WEBP_1X1), { mimeType: 'image/webp', width: 1, height: 1 });
});
