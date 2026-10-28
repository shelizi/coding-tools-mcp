import { getBackend } from "$lib/backend";

/** Only expiry timestamps are returned; no bearer, handle, fingerprint or owner. */
export interface SessionExpirySnapshot {
  capturedAtMs: number;
  runtimeActive: boolean;
  authType: "oauth" | "bearer" | "noauth" | "unknown";
  oauthTokenTtlSeconds: number | null;
  oauthTokenExpiresAtMs: number[];
  conversationHandleExpiresAtMs: number[];
  scope: "current_runtime";
}

export async function getSessionExpiry(workspaceId: string): Promise<SessionExpirySnapshot> {
  return getBackend().sessionExpiry.read(workspaceId);
}

export function remainingMs(expiresAtMs: number, now: number): number {
  return Math.max(0, expiresAtMs - now);
}

export function formatRemaining(expiresAtMs: number, now: number): string {
  const seconds = Math.ceil(remainingMs(expiresAtMs, now) / 1000);
  const days = Math.floor(seconds / 86400);
  const hours = Math.floor((seconds % 86400) / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  const rest = seconds % 60;
  const time = [hours, minutes, rest].map(value => String(value).padStart(2, "0")).join(":");
  return days > 0 ? `${days}d ${time}` : time;
}
