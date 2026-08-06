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

export const AGENT_VERSION = readAgentVersion();
