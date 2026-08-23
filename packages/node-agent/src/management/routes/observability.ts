import type { IncomingMessage, ServerResponse } from 'node:http';
import { dashboardPayload } from '../../dashboard.js';
import { managementDirectoryPayload, openManagementDirectory } from '../../managementDirectories.js';
import {
  managementDiagnosticsPayload,
  managementHealthPayload,
  managementHistoryDetailPayload,
  managementHistoryListPayload,
  managementOperationLogPayload,
  managementTelemetryPayload
} from '../../managementObservability.js';
import { sendJson } from '../../oauth.js';
import {
  localListenerBaseUrl,
  readManagementBody,
  sendDirectoryBrowseError,
  sendObservabilityError
} from '../http.js';
import { runtimeRecord } from '../runtime.js';
import type { ManagementOptions } from '../types.js';

export async function handleManagementObservabilityRoute(
  req: IncomingMessage,
  res: ServerResponse,
  pathname: string,
  options: ManagementOptions
): Promise<boolean> {
  if (pathname === '/admin/api/dashboard' && req.method === 'GET') {
    const requestUrl = new URL(req.url ?? pathname, `http://${req.headers.host ?? '127.0.0.1'}`);
    const workspaceId = requestUrl.searchParams.get('workspaceId')?.trim();
    const record = workspaceId ? options.runtimeRegistry?.get(workspaceId) : undefined;
    if (workspaceId && !record) {
      sendJson(res, 404, {
        error: { code: 'WORKSPACE_NOT_FOUND', message: `Workspace was not found: ${workspaceId}` }
      });
      return true;
    }
    const selected = record ?? {
      context: options.context,
      oauth: options.oauth,
      startedAt: options.startedAt
    };
    sendJson(res, 200, await dashboardPayload(selected.context, selected.startedAt));
    return true;
  }

  if (pathname === '/admin/api/directories/open' && req.method === 'POST') {
    try {
      const body = await readManagementBody(req);
      const input = body && typeof body === 'object' && !Array.isArray(body) ? body as Record<string, unknown> : {};
      sendJson(res, 200, await openManagementDirectory(String(input.path ?? '')));
    } catch (error) {
      sendDirectoryBrowseError(res, error);
    }
    return true;
  }

  if (pathname === '/admin/api/directories' && req.method === 'GET') {
    const requestUrl = new URL(req.url ?? pathname, `http://${req.headers.host ?? '127.0.0.1'}`);
    const workspaceId = requestUrl.searchParams.get('workspaceId')?.trim();
    const selected = workspaceId ? runtimeRecord(options, workspaceId) : undefined;
    if (workspaceId && !selected) {
      sendJson(res, 404, {
        error: { code: 'WORKSPACE_NOT_FOUND', message: `Workspace was not found: ${workspaceId}` }
      });
      return true;
    }
    try {
      sendJson(res, 200, await managementDirectoryPayload(
        selected?.context.config ?? options.context.config,
        requestUrl.searchParams.get('path')
      ));
    } catch (error) {
      sendDirectoryBrowseError(res, error);
    }
    return true;
  }

  const route = pathname.match(
    /^\/admin\/api\/workspaces\/([A-Za-z0-9._-]{1,128})\/(telemetry|logs|history(?:\/([1-9]\d*))?|health|diagnostics)$/
  );
  if (!route) return false;

  const [, workspaceId, action, historyNumber] = route;
  const selected = runtimeRecord(options, workspaceId);
  if (!selected) {
    sendJson(res, 404, {
      error: { code: 'WORKSPACE_NOT_FOUND', message: `Workspace was not found: ${workspaceId}` }
    });
    return true;
  }
  const requestUrl = new URL(req.url ?? pathname, `http://${req.headers.host ?? '127.0.0.1'}`);
  try {
    if (action === 'telemetry' && req.method === 'GET') {
      sendJson(res, 200, await managementTelemetryPayload(selected.context, requestUrl.searchParams));
      return true;
    }
    if (action === 'logs' && req.method === 'GET') {
      sendJson(res, 200, await managementOperationLogPayload(selected.context, requestUrl.searchParams));
      return true;
    }
    if (action === 'history' && req.method === 'GET') {
      sendJson(
        res,
        200,
        await managementHistoryListPayload(selected.context, requestUrl.searchParams.get('folderId'))
      );
      return true;
    }
    if (historyNumber && req.method === 'GET') {
      sendJson(res, 200, await managementHistoryDetailPayload(
        selected.context,
        Number(historyNumber),
        requestUrl.searchParams.get('folderId')
      ));
      return true;
    }
    if (action === 'health' && req.method === 'POST') {
      sendJson(res, 200, await managementHealthPayload(selected.context, localListenerBaseUrl(req)));
      return true;
    }
    if (action === 'diagnostics' && req.method === 'GET') {
      sendJson(res, 200, await managementDiagnosticsPayload(selected.context, selected.startedAt));
      return true;
    }
  } catch (error) {
    sendObservabilityError(res, error);
    return true;
  }
  return false;
}
