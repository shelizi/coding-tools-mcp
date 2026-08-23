import { timingSafeEqual } from 'node:crypto';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { sendJson } from '../oauth.js';
import { ManagementObservabilityError } from '../managementObservability.js';

export function isLoopbackAddress(value: string | undefined): boolean {
  const address = (value ?? '').toLowerCase().replace(/^::ffff:/, '');
  return address === '127.0.0.1' || address === '::1';
}

export function loopbackHost(value: string | undefined): boolean {
  if (!value || /[\r\n/\\]/.test(value)) return false;
  try {
    const hostname = new URL(`http://${value}`).hostname.replace(/^\[|\]$/g, '').toLowerCase();
    return hostname === '127.0.0.1' || hostname === 'localhost' || hostname === '::1';
  } catch {
    return false;
  }
}

function privateAddress(value: string | undefined): boolean {
  const address = (value ?? '').toLowerCase().replace(/^::ffff:/, '');
  const octets = address.split('.').map(part => Number(part));
  if (octets.length === 4 && octets.every(part => Number.isInteger(part) && part >= 0 && part <= 255)) {
    return octets[0] === 10
      || (octets[0] === 172 && octets[1] >= 16 && octets[1] <= 31)
      || (octets[0] === 192 && octets[1] === 168);
  }
  return address.startsWith('fc') || address.startsWith('fd');
}

function trustPrivateProxyFromEnvironment(): boolean {
  return /^(?:1|true|yes|on)$/i.test(String(process.env.CTMCP_UI_TRUST_PRIVATE_PROXY ?? '').trim());
}

export function managementClientAllowed(
  req: Pick<IncomingMessage, 'headers' | 'socket'>,
  trustPrivateProxy = trustPrivateProxyFromEnvironment()
): boolean {
  if (!loopbackHost(req.headers.host)) return false;
  return isLoopbackAddress(req.socket.remoteAddress)
    || (trustPrivateProxy && privateAddress(req.socket.remoteAddress));
}

export function sameOrigin(req: IncomingMessage): boolean {
  const origin = req.headers.origin;
  if (!origin) return true;
  try {
    return new URL(origin).origin === `http://${String(req.headers.host ?? '')}`;
  } catch {
    return false;
  }
}

export async function readManagementBody(req: IncomingMessage, limit = 512 * 1024): Promise<unknown> {
  const chunks: Buffer[] = [];
  let bytes = 0;
  for await (const chunk of req) {
    const value = Buffer.from(chunk);
    bytes += value.length;
    if (bytes > limit) throw new Error('management request body is too large');
    chunks.push(value);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

export function validAdminToken(req: IncomingMessage, expected: string): boolean {
  const supplied = String(req.headers['x-ctmcp-admin-token'] ?? '');
  const left = Buffer.from(supplied);
  const right = Buffer.from(expected);
  return left.length === right.length && timingSafeEqual(left, right);
}

export function sendManagementError(res: ServerResponse, error: unknown): void {
  sendJson(res, 400, {
    error: { code: 'CONFIG_INVALID', message: error instanceof Error ? error.message : String(error) }
  });
}

export function sendObservabilityError(res: ServerResponse, error: unknown): void {
  if (error instanceof ManagementObservabilityError) {
    sendJson(res, error.status, { error: { code: error.code, message: error.message } });
    return;
  }
  sendJson(res, 500, {
    error: { code: 'OBSERVABILITY_FAILED', message: error instanceof Error ? error.message : String(error) }
  });
}

export function sendDirectoryBrowseError(res: ServerResponse, error: unknown): void {
  const code = error && typeof error === 'object' && 'code' in error ? String(error.code) : '';
  const status = code === 'ENOENT' ? 404 : code === 'EACCES' || code === 'EPERM' ? 403 : 400;
  sendJson(res, status, {
    error: { code: 'DIRECTORY_BROWSE_FAILED', message: error instanceof Error ? error.message : String(error) }
  });
}

export function localListenerBaseUrl(req: IncomingMessage): string {
  const port = req.socket.localPort;
  if (!port) {
    throw new ManagementObservabilityError(
      500,
      'LOCAL_LISTENER_UNAVAILABLE',
      'Local listener address is unavailable.'
    );
  }
  const address = req.socket.localAddress ?? '127.0.0.1';
  const host = address === '0.0.0.0'
    ? '127.0.0.1'
    : address === '::'
      ? '[::1]'
      : address.includes(':')
        ? `[${address}]`
        : address;
  return `http://${host}:${port}`;
}
