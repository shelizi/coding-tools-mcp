export type FrontendHost = "desktop" | "node";

export interface FrontendCapabilities {
  host: FrontendHost;
  actions: boolean;
  frpManagement: boolean;
  nativeDirectoryPicker: boolean;
  softwareManagement: boolean;
  rawRuntimeLogs: boolean;
  agentRestart: boolean;
  staticBearerAuth: boolean;
  liveHistoryActivity: boolean;
  wslFolders: boolean;
  openNativePath: boolean;
  directoryBrowser: boolean;
  operationLogs: boolean;
  workspaceFeatureControls: boolean;
  runtimeSupervisor: boolean;
  sharedSecretStore: boolean;
  guidedSetup: boolean;
  workspaceLifecycle: boolean;
}

export type BooleanCapability = Exclude<keyof FrontendCapabilities, "host">;

export const DESKTOP_CAPABILITIES: FrontendCapabilities = {
  host: "desktop",
  actions: true,
  frpManagement: true,
  nativeDirectoryPicker: true,
  softwareManagement: true,
  rawRuntimeLogs: true,
  agentRestart: false,
  staticBearerAuth: true,
  liveHistoryActivity: true,
  wslFolders: true,
  openNativePath: true,
  directoryBrowser: false,
  operationLogs: false,
  workspaceFeatureControls: true,
  runtimeSupervisor: true,
  sharedSecretStore: true,
  guidedSetup: true,
  workspaceLifecycle: true,
};

export const NODE_CAPABILITIES: FrontendCapabilities = {
  host: "node",
  actions: false,
  frpManagement: false,
  nativeDirectoryPicker: false,
  softwareManagement: false,
  rawRuntimeLogs: false,
  agentRestart: true,
  staticBearerAuth: false,
  liveHistoryActivity: false,
  wslFolders: false,
  openNativePath: true,
  directoryBrowser: true,
  operationLogs: true,
  workspaceFeatureControls: true,
  runtimeSupervisor: false,
  sharedSecretStore: false,
  guidedSetup: true,
  workspaceLifecycle: true,
};
