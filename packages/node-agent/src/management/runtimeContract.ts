import type { OAuthRuntime } from '../oauth.js';
import type { AgentConfig, SandboxConfig, ToolContext } from '../types.js';

export interface TunnelRuntimeController {
  reconfigure(tunnel: AgentConfig['tunnel'], publicBaseUrl?: string): Promise<void>;
  enforceSecurity(): Promise<void>;
  start?(): Promise<void>;
  stop?(): Promise<void>;
}

export interface RuntimeHotApplyTarget {
  context: ToolContext;
  preflightSandbox?: (config: SandboxConfig) => Promise<void>;
  oauth?: OAuthRuntime;
  tunnel?: TunnelRuntimeController;
}
