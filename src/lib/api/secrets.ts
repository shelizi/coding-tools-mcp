import { getBackend, isUnavailableBackendError } from "$lib/backend";

export type WorkspaceSecretKey =
  | "oauth_client_secret"
  | "oauth_password"
  | "oauth_token_secret"
  | "bearer_token"
  | "cloudflare_token"
  | "actions_cloudflare_token"
  | "actions_api_key"
  | "actions_oauth_client_secret"
  | "actions_oauth_password"
  | "actions_oauth_token_secret"
  | "actions_frp_token"
  | "frp_token"
  | "builtin_tunnel_enrollment_url";

export async function getWorkspaceSecret(
  id: string,
  key: WorkspaceSecretKey,
): Promise<string | null> {
  return getBackend().secrets.getWorkspaceSecret(id, key);
}

export async function setWorkspaceSecret(
  id: string,
  key: WorkspaceSecretKey,
  value: string,
): Promise<void> {
  return getBackend().secrets.setWorkspaceSecret(id, key, value);
}

export async function regenerateWorkspaceSecret(
  id: string,
  key: WorkspaceSecretKey,
): Promise<string> {
  return getBackend().secrets.regenerateWorkspaceSecret(id, key);
}

/** @deprecated use WorkspaceSecretKey */
export type SecretKey = WorkspaceSecretKey;

/** @deprecated use getWorkspaceSecret */
export const getSecret = getWorkspaceSecret;

/** @deprecated use setWorkspaceSecret */
export const setSecret = setWorkspaceSecret;

/** @deprecated use regenerateWorkspaceSecret */
export const regenerateSecret = regenerateWorkspaceSecret;

export type SharedSecretKey =
  | "oauth_client_id"
  | "bearer_token"
  | "oauth_client_secret"
  | "oauth_password"
  | "oauth_token_secret"
  | "actions_api_key"
  | "actions_oauth_client_secret"
  | "actions_oauth_password"
  | "actions_oauth_token_secret";

export async function getSharedSecret(key: SharedSecretKey): Promise<string | null> {
  return getBackend().secrets.getSharedSecret(key);
}

export async function setSharedSecret(key: SharedSecretKey, value: string): Promise<void> {
  return getBackend().secrets.setSharedSecret(key, value);
}

export async function regenerateSharedSecret(key: SharedSecretKey): Promise<string> {
  return getBackend().secrets.regenerateSharedSecret(key);
}

export async function secretIsSet(id: string, key: WorkspaceSecretKey): Promise<boolean> {
  try {
    const value = await getWorkspaceSecret(id, key);
    return Boolean(value);
  } catch (error) {
    if (isUnavailableBackendError(error)) return false;
    throw error;
  }
}
