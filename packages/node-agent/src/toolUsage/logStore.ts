import { createReadStream } from 'node:fs';
import { appendFile, mkdir, rename, stat, unlink } from 'node:fs/promises';
import { StringDecoder } from 'node:string_decoder';
import type { JsonObject } from '../types.js';

export interface ToolUsageLogStoreOptions {
  logDir: string;
  logFile: string;
  maxBytes: number;
  retainedFiles: number;
}

function isJsonObject(value: unknown): value is JsonObject {
  return Boolean(value) && typeof value === 'object' && !Array.isArray(value);
}

export class ToolUsageLogStore {
  readonly logDir: string;
  readonly logFile: string;
  readonly maxBytes: number;
  readonly retainedFiles: number;

  constructor(options: ToolUsageLogStoreOptions) {
    this.logDir = options.logDir;
    this.logFile = options.logFile;
    this.maxBytes = options.maxBytes;
    this.retainedFiles = options.retainedFiles;
  }

  async append(record: unknown): Promise<void> {
    const line = `${JSON.stringify(record)}\n`;
    const lineBytes = Buffer.byteLength(line);
    await mkdir(this.logDir, { recursive: true });
    let currentBytes = 0;
    try {
      currentBytes = (await stat(this.logFile)).size;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    if (currentBytes > 0 && currentBytes + lineBytes > this.maxBytes) await this.rotate();
    await appendFile(this.logFile, line, { encoding: 'utf8', mode: 0o600 });
  }

  async visitCompleteRecords(visit: (record: JsonObject) => void): Promise<{
    scannedLines: number;
    invalidLines: number;
    bytesRead: number;
  }> {
    let scannedLines = 0;
    let invalidLines = 0;
    let bytesRead = 0;
    for (const file of this.logPaths()) {
      const decoder = new StringDecoder('utf8');
      let carry = '';
      try {
        const stream = createReadStream(file);
        for await (const chunk of stream) {
          const bytes = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
          bytesRead += bytes.length;
          carry += decoder.write(bytes);
          let newline = carry.indexOf('\n');
          while (newline >= 0) {
            const rawLine = carry.slice(0, newline);
            carry = carry.slice(newline + 1);
            const line = rawLine.endsWith('\r') ? rawLine.slice(0, -1) : rawLine;
            if (line) {
              scannedLines += 1;
              try {
                const parsed = JSON.parse(line);
                if (isJsonObject(parsed)) visit(parsed);
                else invalidLines += 1;
              } catch {
                invalidLines += 1;
              }
            }
            newline = carry.indexOf('\n');
          }
        }
        carry += decoder.end();
        // Intentionally ignore carry: the active writer may have an incomplete JSONL tail.
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code === 'ENOENT') continue;
        throw Object.assign(
          new Error(`Unable to read ${file}: ${error instanceof Error ? error.message : String(error)}`),
          { code: 'LOG_READ_FAILED' }
        );
      }
    }
    return { scannedLines, invalidLines, bytesRead };
  }

  private async rotate(): Promise<void> {
    if (this.retainedFiles <= 0) {
      await unlink(this.logFile).catch(error => {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      });
      return;
    }
    await unlink(`${this.logFile}.${this.retainedFiles}`).catch(error => {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    });
    for (let index = this.retainedFiles - 1; index >= 1; index -= 1) {
      await rename(`${this.logFile}.${index}`, `${this.logFile}.${index + 1}`).catch(error => {
        if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
      });
    }
    await rename(this.logFile, `${this.logFile}.1`).catch(error => {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    });
  }

  private logPaths(): string[] {
    const paths: string[] = [];
    for (let index = this.retainedFiles; index >= 1; index -= 1) {
      paths.push(`${this.logFile}.${index}`);
    }
    paths.push(this.logFile);
    return paths;
  }
}
