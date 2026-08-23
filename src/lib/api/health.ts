import { getBackend } from "$lib/backend";

export interface HealthItem {
  label: string;
  ok: boolean;
  detail: string;
  hint: string;
}

export async function runHealthChecks(workspaceId: string): Promise<HealthItem[]> {
  return getBackend().health.run(workspaceId);
}
