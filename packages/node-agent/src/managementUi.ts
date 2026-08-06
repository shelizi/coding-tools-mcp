import { readFile } from 'node:fs/promises';
import type { IncomingMessage, ServerResponse } from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const uiRoot = fileURLToPath(new URL('./ui/', import.meta.url));
const assets = new Map<string, { file: string; contentType: string; serviceWorker?: boolean }>([
  ['/ui/app.js', { file: 'app.js', contentType: 'text/javascript; charset=utf-8' }],
  ['/ui/app.css', { file: 'app.css', contentType: 'text/css; charset=utf-8' }],
  ['/ui/manifest.webmanifest', { file: 'manifest.webmanifest', contentType: 'application/manifest+json; charset=utf-8' }],
  ['/ui/icon.svg', { file: 'icon.svg', contentType: 'image/svg+xml; charset=utf-8' }],
  ['/ui/sw.js', { file: 'sw.js', contentType: 'text/javascript; charset=utf-8', serviceWorker: true }]
]);

function securityHeaders(contentSecurityPolicy = false): Record<string, string> {
  const headers: Record<string, string> = {
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff',
    'x-frame-options': 'DENY',
    'referrer-policy': 'no-referrer',
    'permissions-policy': 'camera=(), microphone=(), geolocation=()'
  };
  if (contentSecurityPolicy) {
    headers['content-security-policy'] = [
      "default-src 'none'",
      "style-src 'self'",
      "script-src 'self'",
      "connect-src 'self'",
      "img-src 'self' data:",
      "manifest-src 'self'",
      "worker-src 'self'",
      "base-uri 'none'",
      "form-action 'none'",
      "frame-ancestors 'none'"
    ].join('; ');
  }
  return headers;
}

function page(adminToken: string): string {
  if (!/^[A-Za-z0-9_-]+$/.test(adminToken)) throw new Error('Invalid management UI token.');
  return `<!doctype html>
<html lang="zh-Hant" data-bs-theme="light">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<meta name="theme-color" content="#0d6efd">
<meta name="description" content="Coding Tools MCP Headless Agent 管理介面">
<meta name="ctmcp-admin-token" content="${adminToken}">
<link rel="manifest" href="/ui/manifest.webmanifest">
<link rel="icon" href="/ui/icon.svg" type="image/svg+xml">
<link rel="stylesheet" href="/ui/app.css">
<title>Coding Tools MCP 管理介面</title>
</head>
<body>
<div id="root" data-ui-framework="react"></div>
<noscript>此管理介面需要啟用 JavaScript。</noscript>
<script src="/ui/app.js" defer></script>
</body>
</html>`;
}

export function isManagementUiPath(pathname: string): boolean {
  return pathname === '/' || pathname === '/ui' || pathname.startsWith('/ui/');
}

export async function handleManagementUiRequest(
  req: IncomingMessage,
  res: ServerResponse,
  pathname: string,
  adminToken: string
): Promise<boolean> {
  const pagePath = pathname === '/' || pathname === '/ui' || pathname === '/ui/';
  const asset = assets.get(pathname);
  if ((pagePath || asset) && req.method !== 'GET') {
    res.writeHead(405, {
      allow: 'GET',
      'content-type': 'application/json; charset=utf-8',
      ...securityHeaders()
    }).end(JSON.stringify({ error: { code: 'METHOD_NOT_ALLOWED', message: 'Method not allowed.' } }));
    return true;
  }
  if (pathname === '/' && req.method === 'GET') {
    res.writeHead(302, { location: '/ui/', ...securityHeaders() }).end();
    return true;
  }
  if ((pathname === '/ui' || pathname === '/ui/') && req.method === 'GET') {
    res.writeHead(200, { 'content-type': 'text/html; charset=utf-8', ...securityHeaders(true) }).end(page(adminToken));
    return true;
  }
  if (!asset) return false;
  try {
    const content = await readFile(path.join(uiRoot, asset.file));
    const headers: Record<string, string> = { 'content-type': asset.contentType, ...securityHeaders() };
    if (asset.serviceWorker) headers['service-worker-allowed'] = '/ui/';
    res.writeHead(200, headers).end(content);
  } catch (error) {
    const code = (error as NodeJS.ErrnoException).code;
    res.writeHead(code === 'ENOENT' ? 503 : 500, {
      'content-type': 'application/json; charset=utf-8',
      ...securityHeaders()
    }).end(JSON.stringify({
      error: {
        code: code === 'ENOENT' ? 'MANAGEMENT_UI_NOT_BUILT' : 'MANAGEMENT_UI_UNAVAILABLE',
        message: code === 'ENOENT'
          ? 'Management UI assets are unavailable. Run the package build before starting the Agent.'
          : 'Management UI assets could not be loaded.'
      }
    }));
  }
  return true;
}
