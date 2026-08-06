import { decode as decodeJpeg, encode as encodeJpeg } from 'jpeg-js';
import { PNG } from 'pngjs';
import type { JsonObject } from './types.js';

export type ImageMimeType = 'image/png' | 'image/jpeg' | 'image/gif' | 'image/webp';

export interface ImageInfo {
  mimeType: ImageMimeType;
  width: number;
  height: number;
}

export interface RgbaImage {
  width: number;
  height: number;
  data: Buffer;
}

export interface EncodedImage extends ImageInfo {
  data: Buffer;
}

export class ImageContractError extends Error {
  constructor(
    public readonly code: 'BINARY_FILE' | 'OUTPUT_TOO_LARGE',
    message: string,
    public readonly details: JsonObject = {}
  ) {
    super(message);
    this.name = 'ImageContractError';
  }
}

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
const JPEG_SOF_MARKERS = new Set([0xc0, 0xc1, 0xc2, 0xc3, 0xc5, 0xc6, 0xc7, 0xc9, 0xca, 0xcb, 0xcd, 0xce, 0xcf]);
const MAX_DECODE_PIXELS = 100_000_000;
const MAX_DECODE_BYTES = 512 * 1024 * 1024;
const JPEG_QUALITIES = [85, 70, 55, 40] as const;

function binaryFileError(details: JsonObject = {}): ImageContractError {
  return new ImageContractError('BINARY_FILE', 'File is not a supported image.', details);
}

function checkedInfo(mimeType: ImageMimeType, width: number, height: number): ImageInfo {
  if (!Number.isSafeInteger(width) || !Number.isSafeInteger(height) || width <= 0 || height <= 0) {
    throw binaryFileError({ mime_type: mimeType, width, height });
  }
  return { mimeType, width, height };
}

function identifyPng(data: Buffer): ImageInfo | undefined {
  if (data.length < 24 || !data.subarray(0, 8).equals(PNG_SIGNATURE)) return undefined;
  if (data.toString('ascii', 12, 16) !== 'IHDR') throw binaryFileError({ detected_format: 'png' });
  return checkedInfo('image/png', data.readUInt32BE(16), data.readUInt32BE(20));
}

function skipGifSubBlocks(data: Buffer, offsetValue: number): number {
  let offset = offsetValue;
  while (offset < data.length) {
    const length = data[offset];
    offset += 1;
    if (length === 0) return offset;
    if (offset + length > data.length) throw binaryFileError({ detected_format: 'gif' });
    offset += length;
  }
  throw binaryFileError({ detected_format: 'gif' });
}

function identifyGif(data: Buffer): ImageInfo | undefined {
  if (data.length < 13) return undefined;
  const header = data.toString('ascii', 0, 6);
  if (header !== 'GIF87a' && header !== 'GIF89a') return undefined;
  const info = checkedInfo('image/gif', data.readUInt16LE(6), data.readUInt16LE(8));
  const packed = data[10];
  let offset = 13;
  if ((packed & 0x80) !== 0) offset += 3 * (2 ** ((packed & 0x07) + 1));
  if (offset > data.length) throw binaryFileError({ detected_format: 'gif' });
  let sawImage = false;
  while (offset < data.length) {
    const marker = data[offset];
    if (marker === 0x3b) {
      if (!sawImage) throw binaryFileError({ detected_format: 'gif' });
      return info;
    }
    if (marker === 0x21) {
      if (offset + 2 > data.length) throw binaryFileError({ detected_format: 'gif' });
      offset = skipGifSubBlocks(data, offset + 2);
      continue;
    }
    if (marker === 0x2c) {
      if (offset + 10 > data.length) throw binaryFileError({ detected_format: 'gif' });
      const imagePacked = data[offset + 9];
      offset += 10;
      if ((imagePacked & 0x80) !== 0) offset += 3 * (2 ** ((imagePacked & 0x07) + 1));
      if (offset >= data.length) throw binaryFileError({ detected_format: 'gif' });
      offset += 1;
      offset = skipGifSubBlocks(data, offset);
      sawImage = true;
      continue;
    }
    throw binaryFileError({ detected_format: 'gif', marker });
  }
  throw binaryFileError({ detected_format: 'gif' });
}

function identifyJpeg(data: Buffer): ImageInfo | undefined {
  if (data.length < 4 || data[0] !== 0xff || data[1] !== 0xd8) return undefined;
  let offset = 2;
  while (offset < data.length) {
    while (offset < data.length && data[offset] === 0xff) offset += 1;
    if (offset >= data.length) break;
    const marker = data[offset];
    offset += 1;
    if (marker === 0xd9 || marker === 0xda) break;
    if (marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) continue;
    if (offset + 2 > data.length) throw binaryFileError({ detected_format: 'jpeg' });
    const length = data.readUInt16BE(offset);
    if (length < 2 || offset + length > data.length) throw binaryFileError({ detected_format: 'jpeg' });
    if (JPEG_SOF_MARKERS.has(marker)) {
      if (length < 7) throw binaryFileError({ detected_format: 'jpeg' });
      return checkedInfo('image/jpeg', data.readUInt16BE(offset + 5), data.readUInt16BE(offset + 3));
    }
    offset += length;
  }
  throw binaryFileError({ detected_format: 'jpeg' });
}

function uint24Le(data: Buffer, offset: number): number {
  return data[offset] | (data[offset + 1] << 8) | (data[offset + 2] << 16);
}

function identifyWebp(data: Buffer): ImageInfo | undefined {
  if (data.length < 20 || data.toString('ascii', 0, 4) !== 'RIFF' || data.toString('ascii', 8, 12) !== 'WEBP') return undefined;
  const riffEnd = data.readUInt32LE(4) + 8;
  if (riffEnd < 20 || riffEnd > data.length) throw binaryFileError({ detected_format: 'webp' });
  let offset = 12;
  let info: ImageInfo | undefined;
  let sawImage = false;
  while (offset + 8 <= riffEnd) {
    const chunk = data.toString('ascii', offset, offset + 4);
    const size = data.readUInt32LE(offset + 4);
    const start = offset + 8;
    const end = start + size;
    if (end > riffEnd) throw binaryFileError({ detected_format: 'webp', chunk });
    if (chunk === 'VP8X') {
      if (size < 10) throw binaryFileError({ detected_format: 'webp', chunk });
      info = checkedInfo('image/webp', uint24Le(data, start + 4) + 1, uint24Le(data, start + 7) + 1);
    } else if (chunk === 'VP8L') {
      if (size < 5 || data[start] !== 0x2f) throw binaryFileError({ detected_format: 'webp', chunk });
      const width = 1 + data[start + 1] + ((data[start + 2] & 0x3f) << 8);
      const height = 1 + ((data[start + 2] & 0xc0) >> 6) + (data[start + 3] << 2) + ((data[start + 4] & 0x0f) << 10);
      info = checkedInfo('image/webp', width, height);
      sawImage = true;
    } else if (chunk === 'VP8 ') {
      if (size < 10 || data[start + 3] !== 0x9d || data[start + 4] !== 0x01 || data[start + 5] !== 0x2a) {
        throw binaryFileError({ detected_format: 'webp', chunk });
      }
      info = checkedInfo('image/webp', data.readUInt16LE(start + 6) & 0x3fff, data.readUInt16LE(start + 8) & 0x3fff);
      sawImage = true;
    } else if (chunk === 'ANMF') {
      if (size < 16) throw binaryFileError({ detected_format: 'webp', chunk });
      sawImage = true;
    }
    offset = end + (size % 2);
  }
  if (!info || !sawImage) throw binaryFileError({ detected_format: 'webp' });
  return info;
}

export function identifyImage(data: Buffer): ImageInfo {
  return identifyPng(data) ?? identifyJpeg(data) ?? identifyGif(data) ?? identifyWebp(data) ?? (() => { throw binaryFileError(); })();
}

function ensureDecodableSize(info: ImageInfo): void {
  const pixels = info.width * info.height;
  if (!Number.isSafeInteger(pixels) || pixels > MAX_DECODE_PIXELS || pixels * 4 > MAX_DECODE_BYTES) {
    throw binaryFileError({ mime_type: info.mimeType, width: info.width, height: info.height, reason: 'decoded image is too large' });
  }
}

export function decodeRaster(data: Buffer, info: ImageInfo): RgbaImage | undefined {
  if (info.mimeType !== 'image/png' && info.mimeType !== 'image/jpeg') return undefined;
  ensureDecodableSize(info);
  try {
    if (info.mimeType === 'image/png') {
      const decoded = PNG.sync.read(data, { checkCRC: true });
      return { width: decoded.width, height: decoded.height, data: Buffer.from(decoded.data) };
    }
    const decoded = decodeJpeg(data, {
      useTArray: false,
      formatAsRGBA: true,
      tolerantDecoding: false,
      maxResolutionInMP: 100,
      maxMemoryUsageInMB: 512
    });
    return { width: decoded.width, height: decoded.height, data: Buffer.from(decoded.data) };
  } catch (error) {
    throw binaryFileError({
      mime_type: info.mimeType,
      decoder_error: error instanceof Error ? error.message : String(error)
    });
  }
}

export function shouldResize(bytes: number, info: ImageInfo, maxBytes: number, maxWidth: number, maxHeight: number): boolean {
  return bytes > maxBytes || info.width > maxWidth || info.height > maxHeight;
}

function targetDimensions(width: number, height: number, maxWidth: number, maxHeight: number): { width: number; height: number } {
  const scale = Math.min(1, maxWidth / width, maxHeight / height);
  return {
    width: Math.max(1, Math.floor(width * scale)),
    height: Math.max(1, Math.floor(height * scale))
  };
}

function bilinearResize(source: RgbaImage, width: number, height: number): RgbaImage {
  if (source.width === width && source.height === height) return { ...source, data: Buffer.from(source.data) };
  const output = Buffer.allocUnsafe(width * height * 4);
  for (let y = 0; y < height; y += 1) {
    const sourceY = ((y + 0.5) * source.height / height) - 0.5;
    const y0 = Math.max(0, Math.min(source.height - 1, Math.floor(sourceY)));
    const y1 = Math.max(0, Math.min(source.height - 1, y0 + 1));
    const fy = Math.max(0, Math.min(1, sourceY - Math.floor(sourceY)));
    for (let x = 0; x < width; x += 1) {
      const sourceX = ((x + 0.5) * source.width / width) - 0.5;
      const x0 = Math.max(0, Math.min(source.width - 1, Math.floor(sourceX)));
      const x1 = Math.max(0, Math.min(source.width - 1, x0 + 1));
      const fx = Math.max(0, Math.min(1, sourceX - Math.floor(sourceX)));
      const weights = [(1 - fx) * (1 - fy), fx * (1 - fy), (1 - fx) * fy, fx * fy];
      const offsets = [
        (y0 * source.width + x0) * 4,
        (y0 * source.width + x1) * 4,
        (y1 * source.width + x0) * 4,
        (y1 * source.width + x1) * 4
      ];
      let alpha = 0;
      const premultiplied = [0, 0, 0];
      for (let index = 0; index < 4; index += 1) {
        const sourceAlpha = source.data[offsets[index] + 3];
        alpha += sourceAlpha * weights[index];
        for (let channel = 0; channel < 3; channel += 1) {
          premultiplied[channel] += source.data[offsets[index] + channel] * (sourceAlpha / 255) * weights[index];
        }
      }
      const destination = (y * width + x) * 4;
      output[destination + 3] = Math.max(0, Math.min(255, Math.round(alpha)));
      for (let channel = 0; channel < 3; channel += 1) {
        output[destination + channel] = alpha <= 0 ? 0 : Math.max(0, Math.min(255, Math.round(premultiplied[channel] * 255 / alpha)));
      }
    }
  }
  return { width, height, data: output };
}

function encodePng(image: RgbaImage): Buffer {
  const png = new PNG({ width: image.width, height: image.height });
  png.data = Buffer.from(image.data);
  return PNG.sync.write(png, { colorType: 6, inputColorType: 6, inputHasAlpha: true });
}

function encodeJpegWithin(image: RgbaImage, maxBytes: number): EncodedImage | undefined {
  for (const quality of JPEG_QUALITIES) {
    const data = encodeJpeg({ width: image.width, height: image.height, data: image.data }, quality).data;
    if (data.length <= maxBytes) return { data, mimeType: 'image/jpeg', width: image.width, height: image.height };
  }
  return undefined;
}

export function resizeDecodedImage(
  raster: RgbaImage,
  original: ImageInfo,
  maxWidth: number,
  maxHeight: number,
  maxBytes: number
): EncodedImage | undefined {
  const target = targetDimensions(raster.width, raster.height, maxWidth, maxHeight);
  const resized = bilinearResize(raster, target.width, target.height);
  if (original.mimeType === 'image/png') {
    const png = encodePng(resized);
    if (png.length <= maxBytes) return { data: png, mimeType: 'image/png', width: resized.width, height: resized.height };
  }
  return encodeJpegWithin(resized, maxBytes);
}

export function outputTooLarge(maxBytes: number, actualBytes: number, details: JsonObject = {}): ImageContractError {
  return new ImageContractError('OUTPUT_TOO_LARGE', 'Image exceeds max_bytes.', { max_bytes: maxBytes, actual_bytes: actualBytes, ...details });
}
