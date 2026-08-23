import { getBackend } from "$lib/backend";

export interface SoftwareStatus {
  kind: string;
  name: string;
  installed: boolean;
  path: string;
  managed: boolean;
  group?: "tunnel" | "sandbox" | string;
  installable?: boolean;
  hint?: string;
  nextSteps?: string;
}

export interface DownloadConfig {
  githubMirror: string;
  proxyMode: string;
  proxyUrl: string;
}

export async function listSoftware(): Promise<SoftwareStatus[]> {
  return getBackend().software.list();
}

export async function installSoftware(kind: string): Promise<SoftwareStatus> {
  return getBackend().software.install(kind);
}

export async function uninstallSoftware(kind: string): Promise<SoftwareStatus> {
  return getBackend().software.uninstall(kind);
}

export async function getDownloadConfig(): Promise<DownloadConfig> {
  return getBackend().software.getDownloadConfig();
}

export async function setDownloadConfig(config: DownloadConfig): Promise<void> {
  return getBackend().software.setDownloadConfig(config);
}
