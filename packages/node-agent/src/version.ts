import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

export { CLIENT_COMPAT_VERSION } from './clientVersion.generated.js';

function readAgentVersion(): string {
  const packagePath = fileURLToPath(new URL('../package.json', import.meta.url));
  const metadata = JSON.parse(readFileSync(packagePath, 'utf8')) as { version?: unknown };
  if (typeof metadata.version !== 'string' || metadata.version.length === 0) {
    throw new Error(`Node Agent package version is missing or invalid: ${packagePath}`);
  }
  return metadata.version;
}

function readBuildInfo(): { buildGitSha: string; sourceClean: boolean | null } {
  const infoPath = fileURLToPath(new URL('./build-info.json', import.meta.url));
  try {
    const metadata = JSON.parse(readFileSync(infoPath, 'utf8')) as { buildGitSha?: unknown; sourceClean?: unknown };
    const value = typeof metadata.buildGitSha === 'string' ? metadata.buildGitSha.trim().toLowerCase() : '';
    return {
      buildGitSha: /^[0-9a-f]{40}$/.test(value) ? value : 'unknown',
      sourceClean: typeof metadata.sourceClean === 'boolean' ? metadata.sourceClean : null
    };
  } catch {
    return { buildGitSha: 'unknown', sourceClean: null };
  }
}

const BUILD_INFO = readBuildInfo();
export const AGENT_VERSION = readAgentVersion();
export const BUILD_GIT_SHA = BUILD_INFO.buildGitSha;
export const BUILD_SOURCE_CLEAN = BUILD_INFO.sourceClean;
