import { runBuffered, type BufferedRunRouting } from './processes.js';

export function gitRunRouting(): BufferedRunRouting {
  return {
    routeWsl: true,
    environment: [['GIT_TERMINAL_PROMPT', '0']]
  };
}

export function runGitBuffered(
  cwd: string,
  args: string[],
  input?: string,
  timeoutMs = 30_000
): Promise<{ code: number | null; stdout: string; stderr: string }> {
  return runBuffered(
    'git',
    args,
    cwd,
    input,
    timeoutMs,
    { ...process.env, GIT_TERMINAL_PROMPT: '0' },
    gitRunRouting()
  );
}
