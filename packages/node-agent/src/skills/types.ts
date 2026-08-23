export type SkillSource = 'project' | 'agents' | 'claude' | 'codex-user' | 'claude-user';
export type SkillScope = 'workspace' | 'user';

export interface SkillDescriptor {
  key: string;
  name: string;
  description: string;
  source: SkillSource;
  scope: SkillScope;
  precedence: number;
  entrypoint: string;
  relativePath: string;
  root: string;
  rootRelativePath: string;
  body: string;
  content: string;
  contentSha256: string;
  version?: string;
  sizeBytes: number;
}

export interface SkillSummary {
  name: string;
  description: string;
  source: SkillSource;
  scope: SkillScope;
  relative_path: string;
  root_relative_path: string;
  content_sha256: string;
  version?: string;
}

export interface SkillDiagnostic {
  code: string;
  message: string;
  path?: string;
  name?: string;
  source?: SkillSource;
  scope?: SkillScope;
}

export interface SkillSnapshot {
  skills: readonly SkillDescriptor[];
  diagnostics: readonly SkillDiagnostic[];
  revision: string;
  scannedAtMs: number;
}

export interface SkillInventoryItem {
  skill: SkillDescriptor;
  selected: boolean;
  enabled: boolean;
}

export interface SkillInventorySnapshot {
  skills: readonly SkillInventoryItem[];
  diagnostics: readonly SkillDiagnostic[];
  scannedAtMs: number;
}

export function skillSummary(skill: SkillDescriptor): SkillSummary {
  return {
    name: skill.name,
    description: skill.description,
    source: skill.source,
    scope: skill.scope,
    relative_path: skill.relativePath,
    root_relative_path: skill.rootRelativePath,
    content_sha256: skill.contentSha256,
    ...(skill.version ? { version: skill.version } : {})
  };
}
