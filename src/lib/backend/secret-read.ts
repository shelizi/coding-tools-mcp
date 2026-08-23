import { CapabilityError, UnimplementedError } from "./errors";
import type {
  FrontendBackend,
  FrontendCapabilities,
  SharedSecretKey,
  WorkspaceSecretKey,
} from "./types";

export function isUnavailableBackendError(error: unknown): boolean {
  return error instanceof UnimplementedError || error instanceof CapabilityError;
}

export async function readSecretIfAvailable(
  load: () => Promise<string | null>,
): Promise<string> {
  try {
    return (await load()) ?? "";
  } catch (error) {
    if (isUnavailableBackendError(error)) return "";
    throw error;
  }
}

export function workspaceAuthSecretKeys(
  authType: string,
  capabilities: FrontendCapabilities,
): WorkspaceSecretKey[] {
  if (authType === "oauth") {
    return capabilities.staticBearerAuth
      ? ["oauth_client_secret", "oauth_password"]
      : ["oauth_password"];
  }
  if (authType === "bearer" && capabilities.staticBearerAuth) return ["bearer_token"];
  return [];
}

export interface McpAuthSecrets {
  oauth_client_id: string;
  oauth_client_secret: string;
  oauth_password: string;
  bearer_token: string;
}

export async function loadMcpAuthSecrets(
  backend: FrontendBackend,
  workspaceId: string,
  auth: { type: string; oauth_client_id?: string; use_shared_secrets?: boolean },
): Promise<McpAuthSecrets> {
  const empty: McpAuthSecrets = {
    oauth_client_id: auth.oauth_client_id ?? "",
    oauth_client_secret: "",
    oauth_password: "",
    bearer_token: "",
  };
  const useShared = Boolean(auth.use_shared_secrets) && backend.capabilities.sharedSecretStore;
  const keys = workspaceAuthSecretKeys(auth.type, backend.capabilities);

  if (auth.type === "oauth" && useShared) {
    empty.oauth_client_id = await readSecretIfAvailable(() =>
      backend.secrets.getSharedSecret("oauth_client_id"),
    );
  }

  const loaded = await Promise.all(
    keys.map(async (key) => {
      const value = useShared
        ? await readSecretIfAvailable(() => backend.secrets.getSharedSecret(key as SharedSecretKey))
        : await readSecretIfAvailable(() => backend.secrets.getWorkspaceSecret(workspaceId, key));
      return [key, value] as const;
    }),
  );
  return { ...empty, ...Object.fromEntries(loaded) };
}
