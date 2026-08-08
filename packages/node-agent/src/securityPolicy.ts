import type { PermissionMode, SecurityPolicy, ToolProfileSetting } from './types.js';

export const securityPolicyKeys = [
  'restrictToolCatalog',
  'enforceCommandAllowlist',
  'requireDangerousConfirmation',
  'requireShellConfirmation',
  'blockNetworkCommands',
  'enforceWorkspaceBoundary',
  'protectRepositoryMetadata',
  'blockSymlinkEscape',
  'protectEnvironmentVariables',
  'enforceHarnessBaseline',
  'requireWriteConfirmation',
  'verifyWriteConflicts',
  'enforceResourceLimits',
  'redactSensitiveOutput',
  'withholdSensitiveSourceOutput',
  'redactTelemetry',
  'redactHistory'
] as const satisfies readonly (keyof SecurityPolicy)[];

export function legacySecurityPolicy(
  permissionMode: PermissionMode | string = 'trusted',
  toolProfile: ToolProfileSetting | string = 'core'
): SecurityPolicy {
  const mode = String(permissionMode);
  const profile = String(toolProfile);
  return {
    restrictToolCatalog: !['advanced', 'compat-readonly-all'].includes(profile),
    enforceCommandAllowlist: true,
    requireDangerousConfirmation: mode !== 'dangerous',
    requireShellConfirmation: mode !== 'dangerous',
    blockNetworkCommands: !['trusted', 'dangerous'].includes(mode),
    enforceWorkspaceBoundary: true,
    protectRepositoryMetadata: true,
    blockSymlinkEscape: true,
    protectEnvironmentVariables: true,
    enforceHarnessBaseline: true,
    requireWriteConfirmation: true,
    verifyWriteConflicts: true,
    enforceResourceLimits: true,
    redactSensitiveOutput: true,
    withholdSensitiveSourceOutput: true,
    redactTelemetry: true,
    redactHistory: true
  };
}

export function normalizeSecurityPolicy(
  value: Partial<SecurityPolicy> | undefined,
  permissionMode: PermissionMode | string = 'trusted',
  toolProfile: ToolProfileSetting | string = 'core'
): SecurityPolicy {
  const defaults = legacySecurityPolicy(permissionMode, toolProfile);
  if (!value || typeof value !== 'object' || Array.isArray(value)) return defaults;
  const normalized = { ...defaults };
  for (const key of securityPolicyKeys) {
    const candidate = value[key];
    if (typeof candidate === 'boolean') normalized[key] = candidate;
  }
  return normalized;
}

export function compatibilityPermissionMode(policy: SecurityPolicy): PermissionMode {
  if (!policy.requireDangerousConfirmation && !policy.requireShellConfirmation && !policy.requireWriteConfirmation) {
    return 'dangerous';
  }
  return policy.blockNetworkCommands ? 'guarded' : 'trusted';
}

export function compatibilityToolProfile(policy: SecurityPolicy): ToolProfileSetting {
  return policy.restrictToolCatalog ? 'trusted-core' : 'advanced';
}
