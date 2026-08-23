import { getBackend } from "$lib/backend";

export interface LogChunk {
  name: string;
  content: string;
}

export type LogService = "mcp" | "actions";

export async function readWorkspaceLogs(
  workspaceId: string,
  service: LogService,
): Promise<LogChunk[]> {
  return getBackend().logs.readRaw(workspaceId, service);
}
