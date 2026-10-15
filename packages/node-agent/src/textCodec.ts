import { createReadStream } from 'node:fs';
import { open, readFile, stat } from 'node:fs/promises';
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

function binaryFileError(): TextDecodingError {
  return new TextDecodingError('BINARY_FILE', 'Binary file read blocked for text tool.');
}

const BINARY_FILE_EXTENSIONS = new Set([
  'png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'ico', 'tif', 'tiff', 'heic', 'avif',
  'mp4', 'm4v', 'mov', 'mkv', 'webm', 'avi', 'wmv',
  'mp3', 'wav', 'flac', 'aac', 'ogg', 'm4a', 'opus',
  'zip', 'tar', 'gz', 'tgz', '7z', 'rar', 'bz2', 'xz', 'zst',
  'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx',
  'ttf', 'otf', 'woff', 'woff2', 'eot',
  'exe', 'dll', 'so', 'dylib', 'o', 'obj', 'a', 'lib', 'wasm',
  'class', 'jar', 'war',
  'pyc', 'pyo',
  'sqlite', 'sqlite3',
  'bin', 'iso', 'dmg', 'img', 'qcow2', 'vmdk',
  'pt', 'pth', 'onnx', 'gguf',
  'parquet',
  'ibd', 'frm', 'myd', 'myi', 'mdb', 'ldb', 'sst', 'wal',
  'apk', 'deb', 'rpm'
]);

const TEXT_FILE_EXTENSIONS = new Set([
  'ts', 'tsx', 'js', 'jsx', 'mjs', 'cjs', 'mts', 'cts',
  'json', 'jsonc', 'json5', 'md', 'markdown', 'mdx', 'rst', 'txt', 'text', 'log', 'tmp',
  'py', 'pyi', 'rb', 'php', 'go', 'rs', 'java', 'kt', 'kts', 'scala',
  'c', 'h', 'cc', 'cpp', 'cxx', 'hpp', 'hxx', 'cs', 'fs', 'swift',
  'sh', 'bash', 'zsh', 'fish', 'ps1', 'psm1', 'bat', 'cmd',
  'yml', 'yaml', 'toml', 'ini', 'cfg', 'conf', 'cnf', 'env',
  'xml', 'html', 'htm', 'xhtml', 'css', 'scss', 'sass', 'less',
  'vue', 'svelte', 'astro', 'graphql', 'gql', 'sql', 'prisma',
  'svg', 'csv', 'tsv', 'properties', 'gradle',
  'r', 'lua', 'pl', 'pm', 'ex', 'exs', 'erl', 'hrl',
  'hs', 'elm', 'clj', 'cljs', 'edn', 'nim', 'zig',
  'proto', 'thrift',
  'gitignore', 'gitattributes', 'dockerignore', 'editorconfig',
  'npmrc', 'nvmrc', 'tf', 'tfvars', 'hcl', 'lock'
]);

const TEXT_FILE_BASENAMES = new Set([
  'dockerfile', 'makefile', 'gnumakefile', 'license', 'copying', 'authors',
  'notice', 'changelog', 'gemfile', 'procfile', 'jenkinsfile', 'vagrantfile',
  'rakefile', 'brewfile', 'caddyfile', 'justfile', 'taskfile', 'readme'
]);

export function binaryFileExtension(filePath: string): boolean {
  const base = filePath.replaceAll('\\', '/').split('/').pop() ?? '';
  const dot = base.lastIndexOf('.');
  if (dot <= 0 || dot === base.length - 1) return false;
  return BINARY_FILE_EXTENSIONS.has(base.slice(dot + 1).toLowerCase());
}

export function searchableTextPath(filePath: string): boolean {
  if (binaryFileExtension(filePath)) return false;
  const base = filePath.replaceAll('\\', '/').split('/').pop() ?? '';
  if (!base) return false;
  const dot = base.lastIndexOf('.');
  if (dot <= 0 || dot === base.length - 1) return TEXT_FILE_BASENAMES.has(base.toLowerCase());
  return TEXT_FILE_EXTENSIONS.has(base.slice(dot + 1).toLowerCase());
}

export function textSearchGlobs(): string[] {
  return [
    ...[...TEXT_FILE_EXTENSIONS].map(ext => `*.${ext}`),
    ...TEXT_FILE_BASENAMES
  ];
}

export function binaryExcludeGlobs(): string[] {
  return [...BINARY_FILE_EXTENSIONS].map(ext => `*.${ext}`);
}

export async function fileLooksBinary(file: string): Promise<boolean> {
  let handle;
  try {
    handle = await open(file, 'r');
    const buf = Buffer.alloc(4096);
    const result = await handle.read(buf, 0, 4096, 0);
    const slice = buf.subarray(0, result.bytesRead);
    return binaryMagicBytes(slice) || slice.includes(0);
  } catch {
    return true;
  } finally {
    await handle?.close();
  }
}

function hasPrefix(data: Uint8Array, prefix: readonly number[], offset = 0): boolean {
  if (data.length < offset + prefix.length) return false;
  for (let index = 0; index < prefix.length; index += 1) {
    if (data[offset + index] !== prefix[index]) return false;
  }
  return true;
}

function hasAscii(data: Uint8Array, offset: number, text: string): boolean {
  if (data.length < offset + text.length) return false;
  for (let index = 0; index < text.length; index += 1) {
    if (data[offset + index] !== text.charCodeAt(index)) return false;
  }
  return true;
}

export function binaryMagicBytes(data: Uint8Array): boolean {
  if (data.length < 2) return false;
  if (hasPrefix(data, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return true;
  if (hasPrefix(data, [0xff, 0xd8, 0xff])) return true;
  if (hasAscii(data, 0, 'GIF87a') || hasAscii(data, 0, 'GIF89a')) return true;
  if (hasAscii(data, 0, 'RIFF') && data.length >= 12) return true;
  if (hasPrefix(data, [0x42, 0x4d])) return true;
  if (hasPrefix(data, [0x00, 0x00, 0x01, 0x00])) return true;
  if (hasPrefix(data, [0x49, 0x49, 0x2a, 0x00]) || hasPrefix(data, [0x4d, 0x4d, 0x00, 0x2a])) return true;
  if (hasAscii(data, 0, 'PK') && data.length >= 4 && (data[2] === 0x03 || data[2] === 0x05 || data[2] === 0x07)) return true;
  if (hasPrefix(data, [0x1f, 0x8b])) return true;
  if (hasPrefix(data, [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c])) return true;
  if (hasAscii(data, 0, 'Rar!')) return true;
  if (hasPrefix(data, [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00])) return true;
  if (hasAscii(data, 0, 'BZh')) return true;
  if (hasAscii(data, 0, '%PDF-')) return true;
  if (hasPrefix(data, [0x7f, 0x45, 0x4c, 0x46])) return true;
  if (
    hasPrefix(data, [0xcf, 0xfa, 0xed, 0xfe])
    || hasPrefix(data, [0xce, 0xfa, 0xed, 0xfe])
    || hasPrefix(data, [0xfe, 0xed, 0xfa, 0xcf])
    || hasPrefix(data, [0xfe, 0xed, 0xfa, 0xce])
  ) return true;
  if (hasAscii(data, 0, 'MZ')) return true;
  if (hasPrefix(data, [0x00, 0x61, 0x73, 0x6d])) return true;
  if (hasAscii(data, 0, 'SQLite format 3')) return true;
  if (hasPrefix(data, [0xca, 0xfe, 0xba, 0xbe])) return true;
  if (data.length >= 8 && hasAscii(data, 4, 'ftyp')) return true;
  if (hasAscii(data, 0, 'OggS') || hasAscii(data, 0, 'fLaC')) return true;
  if (hasPrefix(data, [0x1a, 0x45, 0xdf, 0xa3])) return true;
  return false;
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
  if (binaryMagicBytes(data) || data.subarray(0, 4096).includes(0)) throw binaryFileError();
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

function detectBom(data: Buffer): { encoding: TextEncoding; bomLength: number; bom: boolean } {
  if (data.length >= 3 && data[0] === 0xef && data[1] === 0xbb && data[2] === 0xbf) {
    return { encoding: 'utf-8', bomLength: 3, bom: true };
  }
  if (data.length >= 2 && data[0] === 0xff && data[1] === 0xfe) {
    return { encoding: 'utf-16le', bomLength: 2, bom: true };
  }
  if (data.length >= 2 && data[0] === 0xfe && data[1] === 0xff) {
    return { encoding: 'utf-16be', bomLength: 2, bom: true };
  }
  return { encoding: 'utf-8', bomLength: 0, bom: false };
}

function decoderFor(encoding: TextEncoding): TextDecoder {
  const label = encoding === 'utf-16be' ? 'utf-16be' : encoding === 'utf-16le' ? 'utf-16le' : 'utf-8';
  return new TextDecoder(label, { fatal: true, ignoreBOM: true });
}

/**
 * Yields decoded lines without retaining the file body. Line splitting matches
 * `String.prototype.split(/\\r?\\n/)`, including a trailing empty line when the
 * file ends with a newline.
 */
export async function* streamDecodedTextLines(
  file: string,
  maxBytes = MAX_TEXT_BYTES
): AsyncGenerator<string, void, undefined> {
  const info = await stat(file);
  enforceDecodeLimit(info.size, maxBytes);
  const stream = createReadStream(file, { highWaterMark: 64 * 1024 });
  let decoder: TextDecoder | undefined;
  let pending = '';
  let sawChunk = false;
  let binaryChecked = false;
  let skipNulCheck = false;
  let prefix = Buffer.alloc(0);
  try {
    for await (const chunk of stream) {
      const data = chunk as Buffer;
      let bytes = data;
      if (!sawChunk) {
        sawChunk = true;
        const bom = detectBom(data);
        skipNulCheck = bom.bom;
        bytes = data.subarray(bom.bomLength);
        decoder = decoderFor(bom.encoding);
        if (bom.encoding === 'utf-16le' || bom.encoding === 'utf-16be') {
          if ((info.size - bom.bomLength) % 2 !== 0) throw unsupportedEncoding();
        }
      }
      if (!binaryChecked && !skipNulCheck) {
        prefix = Buffer.concat(prefix.length ? [prefix, data] : [data]);
        if (prefix.length >= 4096 || prefix.length >= info.size) {
          if (binaryMagicBytes(prefix) || prefix.subarray(0, 4096).includes(0)) throw binaryFileError();
          binaryChecked = true;
          prefix = Buffer.alloc(0);
        }
      }
      pending += decoder!.decode(bytes, { stream: true });
      const parts = pending.split(/\r?\n/);
      pending = parts.pop() ?? '';
      for (const line of parts) yield line;
    }
    if (!sawChunk) {
      yield '';
      return;
    }
    if (!binaryChecked && !skipNulCheck && (binaryMagicBytes(prefix) || prefix.subarray(0, 4096).includes(0))) {
      throw binaryFileError();
    }
    pending += decoder!.decode();
    const parts = pending.split(/\r?\n/);
    for (const line of parts) yield line;
  } finally {
    stream.destroy();
  }
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
