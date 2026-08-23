import type { JsonObject } from '../types.js';

export function commandEnvironment(args: JsonObject): NodeJS.ProcessEnv {
  const environment: NodeJS.ProcessEnv = { ...process.env };
  for (const name of removedEnvironment(args)) delete environment[name];
  for (const [name, value] of explicitEnvironment(args)) environment[name] = value;
  return environment;
}

export function explicitEnvironment(args: JsonObject): Array<[string, string]> {
  return Object.entries((args.env as Record<string, unknown> | undefined) ?? {})
    .map(([name, value]) => [name, String(value)]);
}

export function removedEnvironment(args: JsonObject): string[] {
  return Array.isArray(args.remove_env) ? args.remove_env.map(String) : [];
}
