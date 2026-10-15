import type { IncomingMessage, ServerResponse } from 'node:http';
import { sendJson } from '../../oauth.js';
import { readManagementBody, sendManagementError } from '../http.js';
import {
  installRipgrep, listSearchSoftware, readDownloadConfig, uninstallRipgrep, writeDownloadConfig,
  type NodeDownloadConfig
} from '../../searchSoftware.js';
import type { ManagementOptions } from '../types.js';

function dataDir(options: ManagementOptions): string {
  return options.context.config.dataDir;
}

export async function handleManagementSoftwareRoute(
  req: IncomingMessage,
  res: ServerResponse,
  pathname: string,
  options: ManagementOptions
): Promise<boolean> {
  if (pathname === '/admin/api/software' && req.method === 'GET') {
    sendJson(res, 200, listSearchSoftware(dataDir(options)));
    return true;
  }
  if (pathname === '/admin/api/software/install' && req.method === 'POST') {
    try {
      const body = await readManagementBody(req);
      const kind = body && typeof body === 'object' && !Array.isArray(body) ? String((body as { kind?: unknown }).kind ?? '') : '';
      if (kind !== 'rg') throw new Error('Node Agent software management can install ripgrep only.');
      sendJson(res, 200, await installRipgrep(dataDir(options), await readDownloadConfig(dataDir(options))));
    } catch (error) {
      sendManagementError(res, error);
    }
    return true;
  }
  if (pathname === '/admin/api/software/uninstall' && req.method === 'POST') {
    try {
      const body = await readManagementBody(req);
      const kind = body && typeof body === 'object' && !Array.isArray(body) ? String((body as { kind?: unknown }).kind ?? '') : '';
      if (kind !== 'rg') throw new Error('Only a managed ripgrep install can be removed.');
      sendJson(res, 200, await uninstallRipgrep(dataDir(options)));
    } catch (error) {
      sendManagementError(res, error);
    }
    return true;
  }
  if (pathname === '/admin/api/software/download-config' && req.method === 'GET') {
    sendJson(res, 200, await readDownloadConfig(dataDir(options)));
    return true;
  }
  if (pathname === '/admin/api/software/download-config' && req.method === 'PUT') {
    try {
      const body = await readManagementBody(req);
      const input = body && typeof body === 'object' && !Array.isArray(body) ? body as Partial<NodeDownloadConfig> : {};
      sendJson(res, 200, await writeDownloadConfig(dataDir(options), {
        githubMirror: String(input.githubMirror ?? ''),
        proxyMode: String(input.proxyMode ?? 'system'),
        proxyUrl: String(input.proxyUrl ?? '')
      }));
    } catch (error) {
      sendManagementError(res, error);
    }
    return true;
  }
  return false;
}
