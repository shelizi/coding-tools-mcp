import type { IncomingMessage, Server, ServerResponse } from 'node:http';
import type { AgentConfig } from '../types.js';

export async function readRequestBody(req: IncomingMessage, limit = 1024 * 1024): Promise<Buffer> {
  const chunks: Buffer[] = [];
  let size = 0;
  for await (const chunk of req) {
    const value = Buffer.from(chunk);
    size += value.length;
    if (size > limit) throw new Error('request body too large');
    chunks.push(value);
  }
  return Buffer.concat(chunks);
}

export function sendText(
  res: ServerResponse,
  status: number,
  value: string,
  type = 'text/plain; charset=utf-8'
): void {
  res.writeHead(status, {
    'content-type': type,
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff'
  }).end(value);
}

export function routePrefix(config: AgentConfig): string {
  if (!config.publicBaseUrl) return '';
  try {
    const pathname = new URL(config.publicBaseUrl).pathname.replace(/\/$/, '');
    return pathname === '/' ? '' : pathname;
  } catch {
    return '';
  }
}

export function localPath(pathname: string, prefix: string): string {
  if (!prefix) return pathname;
  if (pathname === prefix) return '/';
  if (pathname.startsWith(`${prefix}/`)) return pathname.slice(prefix.length);
  return pathname;
}

export function currentListenerPort(server: Server, fallback: number): number {
  const address = server.address();
  return address && typeof address === 'object' ? address.port : fallback;
}
