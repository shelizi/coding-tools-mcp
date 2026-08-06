import type { PermissionMode, ToolDefinition, ToolProfile, ToolProfileSetting } from './types.js';
import {
  rustCatalog, rustToolAnnotationOverridesByProfile, rustToolNamesByProfile,
  rustToolsetRevisionByProfile
} from './rustCatalog.generated.js';

export const toolProfiles: readonly ToolProfile[] = [
  'advanced', 'read-only', 'compat-readonly-all', 'guarded-core', 'trusted-core'
];
export const configurableToolProfiles: readonly ToolProfileSetting[] = [
  'core', 'trusted-core', 'guarded-core', 'read-only', 'advanced', 'compat-readonly-all'
];

export function configuredToolProfile(value: unknown): ToolProfileSetting {
  const profile = String(value ?? '').trim();
  return configurableToolProfiles.includes(profile as ToolProfileSetting)
    ? profile as ToolProfileSetting
    : 'core';
}

export function normalizeToolProfile(value: unknown): ToolProfile {
  const profile = String(value ?? '').trim();
  if (profile === 'advanced' || profile === 'read-only' || profile === 'compat-readonly-all' || profile === 'guarded-core') return profile;
  return 'trusted-core';
}

export function resolveToolProfile(profile: unknown, permissionMode: PermissionMode | string): ToolProfile {
  const normalized = normalizeToolProfile(profile);
  return normalized === 'trusted-core' && permissionMode !== 'trusted' && permissionMode !== 'dangerous'
    ? 'guarded-core'
    : normalized;
}

const rustToolByName = new Map(rustCatalog.map(tool => [tool.name, tool]));

export function toolsForProfile(profile: unknown): ToolDefinition[] {
  const normalized = normalizeToolProfile(profile);
  const overrides = rustToolAnnotationOverridesByProfile[normalized];
  return rustToolNamesByProfile[normalized].map(name => {
    const base = rustToolByName.get(name);
    if (!base) throw new Error(`Generated profile references an unknown Rust tool: ${name}`);
    const annotations = overrides[name];
    return structuredClone(annotations ? { ...base, annotations } : base);
  });
}

export function toolNamesForProfile(profile: unknown): string[] {
  return [...rustToolNamesByProfile[normalizeToolProfile(profile)]];
}

export function toolsetRevisionForProfile(profile: unknown): string {
  return rustToolsetRevisionByProfile[normalizeToolProfile(profile)];
}

// Backward-compatible complete contract exports for internal callers and tests.
export const tools: ToolDefinition[] = toolsForProfile('advanced');
export const toolNames = tools.map(tool => tool.name);
export const toolsetRevision = toolsetRevisionForProfile('advanced');
export const readOnlyTools = new Set(tools.filter(tool => tool.annotations.readOnlyHint).map(tool => tool.name));
export const mutatingTools = new Set(tools.filter(tool => !tool.annotations.readOnlyHint).map(tool => tool.name));
