import type { JsonObject } from './types.js';

export const PERMISSION_MRTR_RESPONSE_KEY = 'permission_approval';
const PERMISSION_MRTR_STATE_PREFIX = 'permission:';

function objectValue(value: unknown): JsonObject | undefined {
  return value && typeof value === 'object' && !Array.isArray(value) ? value as JsonObject : undefined;
}

export interface PermissionMrtrRetry {
  resumeId: string;
  approved: boolean;
  action: 'accept' | 'decline' | 'cancel';
}

export function permissionInputRequired(structured: JsonObject): JsonObject | undefined {
  if (structured.ok !== false) return undefined;
  const error = objectValue(structured.error);
  if (error?.code !== 'PERMISSION_REQUIRED') return undefined;
  const details = objectValue(error.details);
  const permission = objectValue(details?.permission_request);
  const resumeId = typeof permission?.resume_id === 'string' ? permission.resume_id.trim() : '';
  if (!resumeId) return undefined;
  const toolName = typeof permission?.tool_name === 'string' ? permission.tool_name : 'operation';
  const permissionKind = typeof permission?.permission === 'string' ? permission.permission : 'permission';
  const reason = typeof permission?.reason === 'string'
    ? permission.reason
    : `${toolName} requires approval.`;

  return {
    resultType: 'input_required',
    inputRequests: {
      [PERMISSION_MRTR_RESPONSE_KEY]: {
        method: 'elicitation/create',
        params: {
          mode: 'form',
          message: `${reason} Approve this ${permissionKind} request?`,
          requestedSchema: {
            type: 'object',
            properties: {
              approve: {
                type: 'boolean',
                title: 'Approve operation',
                description: `Allow ${toolName} to continue once.`
              }
            },
            required: ['approve'],
            additionalProperties: false
          }
        }
      }
    },
    requestState: `${PERMISSION_MRTR_STATE_PREFIX}${resumeId}`
  };
}

export function permissionMrtrRetry(params: JsonObject): PermissionMrtrRetry | undefined {
  const requestState = typeof params.requestState === 'string' ? params.requestState : '';
  if (!requestState.startsWith(PERMISSION_MRTR_STATE_PREFIX)) return undefined;
  const resumeId = requestState.slice(PERMISSION_MRTR_STATE_PREFIX.length).trim();
  if (!resumeId) throw new Error('MRTR permission requestState is missing its resume identifier');
  const inputResponses = objectValue(params.inputResponses);
  const response = objectValue(inputResponses?.[PERMISSION_MRTR_RESPONSE_KEY]);
  if (!response) throw new Error(`MRTR permission response '${PERMISSION_MRTR_RESPONSE_KEY}' is required`);
  const action = response.action;
  if (action !== 'accept' && action !== 'decline' && action !== 'cancel') {
    throw new Error('MRTR permission response action must be accept, decline, or cancel');
  }
  if (action !== 'accept') return { resumeId, approved: false, action };
  const content = objectValue(response.content);
  if (typeof content?.approve !== 'boolean') {
    throw new Error('Accepted MRTR permission response must include boolean content.approve');
  }
  return { resumeId, approved: content.approve, action };
}