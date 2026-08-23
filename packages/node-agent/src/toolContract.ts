import type { JsonObject } from './types.js';

export const MAX_MCP_SUMMARY_BYTES = 512;

interface ToolErrorFields {
  code: string;
  message: string;
  category: string;
  retryable: boolean;
  details: JsonObject;
}

function objectValue(value: unknown): JsonObject | undefined {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? value as JsonObject
    : undefined;
}

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' && value ? value : undefined;
}

function booleanValue(value: unknown): boolean | undefined {
  return typeof value === 'boolean' ? value : undefined;
}

export function truncateUtf8(value: string, maxBytes = MAX_MCP_SUMMARY_BYTES): string {
  if (Buffer.byteLength(value) <= maxBytes) return value;
  const suffix = '...';
  const target = Math.max(0, maxBytes - Buffer.byteLength(suffix));
  let output = '';
  let bytes = 0;
  for (const character of value) {
    const size = Buffer.byteLength(character);
    if (bytes + size > target) break;
    output += character;
    bytes += size;
  }
  return `${output}${suffix}`;
}

function normalizedErrorFields(result: JsonObject): ToolErrorFields {
  const raw = objectValue(result.error) ?? {};
  const message = stringValue(raw.message)
    ?? stringValue(result.summary)
    ?? 'Tool call failed.';
  return {
    code: stringValue(raw.code) ?? 'TOOL_FAILED',
    message,
    category: stringValue(raw.category) ?? 'tool',
    retryable: booleanValue(raw.retryable) ?? false,
    details: objectValue(raw.details) ?? {}
  };
}

export function normalizeToolResult(result: JsonObject): JsonObject {
  if (result.ok !== false) return result;
  const error = normalizedErrorFields(result);
  return {
    ...result,
    status: stringValue(result.status) ?? 'error',
    summary: stringValue(result.summary) ?? error.message,
    error
  };
}

export function toolFail(
  code: string,
  message: string,
  category = 'tool',
  retryable = false,
  details: JsonObject = {}
): JsonObject {
  return normalizeToolResult({
    ok: false,
    error: { code, message, category, retryable, details }
  });
}

function structuredToolError(error: unknown): ToolErrorFields | undefined {
  if (!error || typeof error !== 'object') return undefined;
  const value = error as Record<string, unknown>;
  const code = stringValue(value.code);
  if (!code) return undefined;
  const name = error instanceof Error ? error.name : stringValue(value.name) ?? '';
  const category = stringValue(value.category)
    ?? (name === 'PolicyError' ? 'policy'
      : name === 'ConversationRoutingError' ? 'workspace_routing'
        : name === 'HarnessError' ? 'permission'
          : name === 'TextDecodingError' || name === 'WslRoutingError' ? 'validation'
            : code.startsWith('WORKSPACE_') ? 'workspace_routing'
              : code === 'POLICY_REJECTED'
                || code === 'DANGEROUS_OPERATION_REQUIRES_CONFIRMATION'
                || code.endsWith('_NOT_ALLOWED') ? 'policy'
                : 'tool');
  return {
    code,
    message: error instanceof Error ? error.message : stringValue(value.message) ?? code,
    category,
    retryable: booleanValue(value.retryable) ?? false,
    details: objectValue(value.details) ?? {}
  };
}

function codeOnlyError(message: string): ToolErrorFields | undefined {
  const known: Record<string, Omit<ToolErrorFields, 'details'>> = {
    IS_DIRECTORY: {
      code: 'IS_DIRECTORY',
      message: 'Path is a directory.',
      category: 'validation',
      retryable: false
    },
    NOT_A_DIRECTORY: {
      code: 'NOT_A_DIRECTORY',
      message: 'Path is not a directory.',
      category: 'validation',
      retryable: false
    },
    NOT_FOUND: {
      code: 'NOT_FOUND',
      message: 'Path not found.',
      category: 'not_found',
      retryable: false
    },
    ABSOLUTE_PATH_DENIED: {
      code: 'ABSOLUTE_PATH_DENIED',
      message: 'Absolute paths are denied.',
      category: 'security',
      retryable: false
    },
    PATH_OUTSIDE_WORKSPACE: {
      code: 'PATH_OUTSIDE_WORKSPACE',
      message: 'Path escapes the configured workspace.',
      category: 'security',
      retryable: false
    },
    SYMLINK_ESCAPE: {
      code: 'SYMLINK_ESCAPE',
      message: 'Path escapes the configured workspace.',
      category: 'security',
      retryable: false
    }
  };
  const fields = known[message];
  return fields ? { ...fields, details: {} } : undefined;
}

function filesystemError(error: unknown): ToolErrorFields | undefined {
  if (!error || typeof error !== 'object' || !('code' in error)) return undefined;
  const value = error as NodeJS.ErrnoException;
  const fsCode = String(value.code ?? '');
  const details: JsonObject = {
    fs_code: fsCode,
    ...(value.syscall ? { syscall: value.syscall } : {})
  };
  switch (fsCode) {
    case 'ENOENT':
      return { code: 'NOT_FOUND', message: 'Path not found.', category: 'not_found', retryable: false, details };
    case 'ENOTDIR':
      return { code: 'NOT_A_DIRECTORY', message: 'Path component is not a directory.', category: 'validation', retryable: false, details };
    case 'EISDIR':
      return { code: 'IS_DIRECTORY', message: 'Path is a directory.', category: 'validation', retryable: false, details };
    case 'EACCES':
    case 'EPERM':
      return { code: 'PERMISSION_DENIED', message: 'Filesystem access was denied.', category: 'security', retryable: false, details };
    default:
      return undefined;
  }
}

export function toolErrorResult(error: unknown): JsonObject {
  const fs = filesystemError(error);
  if (fs) return toolFail(fs.code, fs.message, fs.category, fs.retryable, fs.details);

  const structured = structuredToolError(error);
  if (structured) {
    return toolFail(
      structured.code,
      structured.message,
      structured.category,
      structured.retryable,
      structured.details
    );
  }

  const message = error instanceof Error ? error.message : String(error);
  const known = codeOnlyError(message);
  if (known) return toolFail(known.code, known.message, known.category, known.retryable, known.details);

  if (/required/i.test(message)) {
    return toolFail('INVALID_ARGUMENT', message, 'validation', false, {});
  }
  if (/^[A-Z][A-Z0-9_]+$/.test(message)) {
    const category = message.startsWith('WORKSPACE_') ? 'workspace_routing'
      : message === 'INVALID_ARGUMENT' || message.startsWith('INVALID_') ? 'validation'
        : 'tool';
    return toolFail(message, message, category, false, {});
  }
  return toolFail('TOOL_FAILED', message, 'tool', false, {});
}

export function mcpResultSummary(toolName: string, structured: JsonObject): string {
  const isError = structured.ok === false;
  if (isError) {
    const error = objectValue(structured.error);
    return truncateUtf8(
      stringValue(error?.message)
        ?? stringValue(structured.summary)
        ?? 'Tool call failed.'
    );
  }

  const explicit = stringValue(structured.summary);
  if (explicit) return truncateUtf8(explicit);

  const returnedCount = typeof structured.returned_count === 'number'
    ? structured.returned_count
    : undefined;
  if (returnedCount !== undefined) {
    return truncateUtf8(`${toolName} completed with ${returnedCount} returned items.`);
  }
  const totalMatches = typeof structured.total_matches === 'number'
    ? structured.total_matches
    : undefined;
  if (totalMatches !== undefined) {
    return truncateUtf8(`${toolName} completed with ${totalMatches} matches.`);
  }
  const commandsExecuted = typeof structured.commands_executed === 'number'
    ? structured.commands_executed
    : undefined;
  if (commandsExecuted !== undefined) {
    return truncateUtf8(`${toolName} completed after executing ${commandsExecuted} commands.`);
  }
  const status = stringValue(structured.status);
  if (status) return truncateUtf8(`${toolName} status: ${status}.`);
  return truncateUtf8(toolName ? `${toolName} completed successfully.` : 'Tool call completed successfully.');
}

function imageContent(value: unknown): JsonObject[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const items = value.filter(item => item && typeof item === 'object') as JsonObject[];
  return items.length === 1 && items[0].type === 'image' ? items : undefined;
}

export function wrapMcpToolResult(
  toolName: string,
  args: JsonObject,
  structured: JsonObject
): JsonObject {
  const normalized = normalizeToolResult(structured);
  const isError = normalized.ok === false;
  const suppliedImages = imageContent(normalized.content);
  const useImage = (toolName === 'view_image' || toolName === 'desktop_screenshot')
    && String(args.output ?? 'mcp_image') === 'mcp_image'
    && !isError
    && suppliedImages !== undefined;

  if (useImage) {
    const structuredContent = { ...normalized };
    delete structuredContent.content;
    delete structuredContent.base64;
    delete structuredContent.data_url;
    return {
      content: suppliedImages,
      structuredContent,
      isError: false
    };
  }

  return {
    content: [{ type: 'text', text: mcpResultSummary(toolName, normalized) }],
    structuredContent: normalized,
    isError
  };
}
