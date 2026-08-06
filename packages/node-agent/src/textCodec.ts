import { readFile, stat } from 'node:fs/promises';
import type { JsonObject } from './types.js';

export const MAX_TEXT_BYTES = 2 * 1024 * 1024;
export const MAX_DECODE_BYTES = 128 * 1024 * 1024;

export type TextEncoding = 'utf-8' | 'utf-16le' | 'utf-16be';

export interface DecodedText {
  text: string;
  encoding: TextEncoding;
  bom: boolean;
}

export interface DecodedTextFile extends DecodedText {
  bytes: Buffer;
}

export class TextDecodingError extends Error {
  constructor(
    readonly code: 'BINARY_FILE' | 'UNSUPPORTED_ENCODING' | 'FILE_TOO_LARGE',
    message: string,
    readonly category: 'validation' | 'limit' = 'validation',
    readonly retryable = false,
    readonly details: JsonObject = {}
  ) {
    super(message);
    this.name = 'TextDecodingError';
  }
}

function unsupportedEncoding(): TextDecodingError {
  return new TextDecodingError(
    'UNSUPPORTED_ENCODING',
    'File encoding is not supported; expected UTF-8 or BOM-marked UTF-16.'
  );
}

function decodeFatal(label: 'utf-8' | 'utf-16le' | 'utf-16be', bytes: Buffer): string {
  try {
    return new TextDecoder(label, { fatal: true, ignoreBOM: true }).decode(bytes);
  } catch {
    throw unsupportedEncoding();
  }
}

function enforceDecodeLimit(byteLength: number, maxBytes: number): void {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 0) {
    throw new TextDecodingError('FILE_TOO_LARGE', 'Text decode byte limit is invalid.', 'limit', false, { max_bytes: maxBytes });
  }
  if (byteLength > maxBytes) {
    throw new TextDecodingError(
      'FILE_TOO_LARGE',
      `File exceeds the ${maxBytes} byte text decode limit.`,
      'limit',
      true,
      { total_bytes: byteLength, max_bytes: maxBytes }
    );
  }
}

export function decodeTextBuffer(data: Buffer, maxBytes = MAX_DECODE_BYTES): DecodedText {
  enforceDecodeLimit(data.length, maxBytes);
  if (data.length >= 3 && data[0] === 0xef && data[1] === 0xbb && data[2] === 0xbf) {
    return { text: decodeFatal('utf-8', data.subarray(3)), encoding: 'utf-8', bom: true };
  }
  if (data.length >= 2 && data[0] === 0xff && data[1] === 0xfe) {
    if ((data.length - 2) % 2 !== 0) throw unsupportedEncoding();
    return { text: decodeFatal('utf-16le', data.subarray(2)), encoding: 'utf-16le', bom: true };
  }
  if (data.length >= 2 && data[0] === 0xfe && data[1] === 0xff) {
    if ((data.length - 2) % 2 !== 0) throw unsupportedEncoding();
    return { text: decodeFatal('utf-16be', data.subarray(2)), encoding: 'utf-16be', bom: true };
  }
  if (data.subarray(0, 4096).includes(0)) {
    throw new TextDecodingError('BINARY_FILE', 'Binary file read blocked for text tool.');
  }
  return { text: decodeFatal('utf-8', data), encoding: 'utf-8', bom: false };
}

function swapUtf16Bytes(bytes: Buffer): Buffer {
  const output = Buffer.allocUnsafe(bytes.length);
  for (let index = 0; index < bytes.length; index += 2) {
    output[index] = bytes[index + 1];
    output[index + 1] = bytes[index];
  }
  return output;
}

export function encodeText(text: string, encoding: TextEncoding, bom: boolean): Buffer {
  if (encoding === 'utf-8') {
    const body = Buffer.from(text, 'utf8');
    return bom ? Buffer.concat([Buffer.from([0xef, 0xbb, 0xbf]), body]) : body;
  }
  const littleEndian = Buffer.from(text, 'utf16le');
  if (encoding === 'utf-16le') {
    return bom ? Buffer.concat([Buffer.from([0xff, 0xfe]), littleEndian]) : littleEndian;
  }
  const bigEndian = swapUtf16Bytes(littleEndian);
  return bom ? Buffer.concat([Buffer.from([0xfe, 0xff]), bigEndian]) : bigEndian;
}

export async function readDecodedTextFile(file: string, maxBytes = MAX_TEXT_BYTES): Promise<DecodedTextFile> {
  const info = await stat(file);
  enforceDecodeLimit(info.size, maxBytes);
  const bytes = await readFile(file);
  enforceDecodeLimit(bytes.length, maxBytes);
  return { ...decodeTextBuffer(bytes, maxBytes), bytes };
}

export function textDecodingErrorValue(error: TextDecodingError): JsonObject {
  return {
    code: error.code,
    message: error.message,
    category: error.category,
    retryable: error.retryable,
    details: error.details
  };
}
