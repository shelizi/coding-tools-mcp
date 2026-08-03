export type RuntimeState = "stopped" | "starting" | "running" | "stopping" | "error";

export const DEFAULT_SERVICE_PORT = 28766;
export const DEFAULT_ACTIONS_PORT = 8787;

export interface TunnelConfig {
  type: string;
  public_url: string;
  frp_server: string;
  frp_subdomain: string;
  frp_profile_id?: string;
  frp_server_port?: number;
  cloudflare_mode: string;
  use_proxy?: boolean;
}

export interface AuthConfig {
  type: string;
  oauth_client_id: string;
  use_shared_secrets?: boolean;
}

export interface RuntimeConfig {
  local_port: number;
  bind_address?: string;
  transport_mode?: "streamable-http" | "legacy-json";
  tool_profile: string;
  permission_mode: string;
  runtime_command?: string;
  allowed_commands?: string;
  workspace_local_entries?: boolean;
  workspace_script_extensions?: string;
  blocking_admission_limit?: number;
  process_admission_limit?: number;
  global_blocking_admission_limit?: number;
  global_process_admission_limit?: number;
  active_session_limit?: number;
}

export interface ActionsConfig {
  public_url: string;
  tunnel_type: string;
  frp_server: string;
  frp_subdomain: string;
  frp_profile_id?: string;
  frp_server_port?: number;
  cloudflare_mode: string;
  cloudflare_token?: string;
  use_proxy?: boolean;
  local_port: number;
  bind_address?: string;
  permission_mode: string;
  runtime_command?: string;
  auth_type: string;
  oauth_client_id?: string;
  oauth_scopes?: string;
  allowed_commands?: string;
  max_patch_bytes?: number;
  use_shared_secrets?: boolean;
}

export interface WorkspaceFolder {
  id: string;
  name: string;
  path: string;
  execution?: WorkspaceExecutionTarget;
}

export type WorkspaceExecutionTarget =
  | { kind: "host" }
  | { kind: "wsl"; distro: string; linux_path: string };

export interface WorkspaceProfile {
  id: string;
  name: string;
  path: string;
  folders?: WorkspaceFolder[];
  active_folder_id?: string;
  tunnel: TunnelConfig;
  auth: AuthConfig;
  runtime: RuntimeConfig;
  actions?: ActionsConfig;
}

export function workspaceFolders(profile: WorkspaceProfile): WorkspaceFolder[] {
  if (profile.folders?.length) return profile.folders;
  return [
    {
      id: `legacy-${profile.id}`,
      name: profile.path.replace(/\\/g, "/").split("/").filter(Boolean).at(-1) ?? "workspace",
      path: profile.path,
    },
  ];
}

export function activeWorkspaceFolder(profile: WorkspaceProfile): WorkspaceFolder {
  const folders = workspaceFolders(profile);
  return folders.find((folder) => folder.id === profile.active_folder_id) ?? folders[0];
}

export interface RuntimeStatus {
  state: RuntimeState;
  pid: number | null;
  localMessage: string;
  publicMessage: string;
  localEndpoint: string;
  publicEndpoint: string;
}

export function actionsConfig(profile: WorkspaceProfile): ActionsConfig {
  return {
    public_url: "",
    tunnel_type: "frp",
    frp_server: "",
    frp_subdomain: "",
    cloudflare_mode: "quick",
    local_port: DEFAULT_ACTIONS_PORT,
    bind_address: "127.0.0.1",
    permission_mode: "trusted",
    auth_type: "api_key",
    allowed_commands:
      "pytest,python,python3,npm,npx,node,pnpm,yarn,make,mvn,mvnw,gradle,gradlew,cargo,go,ruff,mypy,eslint,tsc",
    max_patch_bytes: 200_000,
    ...profile.actions,
  };
}

function connectHostForBindAddress(bindAddress = "127.0.0.1"): string {
  const address = bindAddress.trim() || "127.0.0.1";
  if (address === "0.0.0.0") return "127.0.0.1";
  if (address === "::") return "[::1]";
  return address.includes(":") ? `[${address}]` : address;
}

export function mcpLocalEndpoint(port: number, bindAddress = "127.0.0.1"): string {
  return `http://${connectHostForBindAddress(bindAddress)}:${port}/mcp`;
}

export function actionsLocalEndpoint(port: number, bindAddress = "127.0.0.1"): string {
  return `http://${connectHostForBindAddress(bindAddress)}:${port}`;
}

export interface ActionsAuthDraft {
  authType: string;
  oauthClientId: string;
  oauthScopes: string;
  useSharedSecrets?: boolean;
}

export interface FrpProfileSummary {
  id: string;
  name: string;
  server: string;
  serverPort: number;
}

export function frpPublicUrl(
  tunnelType: string,
  frpSubdomain: string,
  frpServer: string,
  frpProfileId: string | undefined,
  profiles: FrpProfileSummary[],
  publicUrl = "",
): string {
  if (tunnelType !== "frp" || !frpSubdomain) {
    return publicUrl.replace(/\/$/, "");
  }
  const server =
    profiles.find((profile) => profile.id === frpProfileId)?.server ?? frpServer;
  if (!server) return publicUrl.replace(/\/$/, "");
  return `https://${frpSubdomain}.${server}`;
}

export function actionsPublicBaseUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const actions = actionsConfig(profile);
  const publicUrl = frpPublicUrl(
    actions.tunnel_type,
    actions.frp_subdomain,
    actions.frp_server,
    actions.frp_profile_id,
    frpProfiles,
    actions.public_url,
  );
  if (publicUrl) return publicUrl;
  return actionsLocalEndpoint(actions.local_port, actions.bind_address);
}

export function actionsOpenApiUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/openapi.json` : "";
}

export function actionsPrivacyUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/privacy` : "";
}

export function actionsOAuthAuthorizeUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/oauth/authorize` : "";
}

export function actionsOAuthTokenUrl(
  profile: WorkspaceProfile,
  frpProfiles: FrpProfileSummary[] = [],
): string {
  const base = actionsPublicBaseUrl(profile, frpProfiles);
  return base ? `${base.replace(/\/$/, "")}/oauth/token` : "";
}
