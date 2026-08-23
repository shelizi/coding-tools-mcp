import type { JsonObject, ProcessSession, ToolContext } from '../types.js';
import { relativeInside, rootAndCwd } from '../workspace.js';
import { resolveCommandSpec, resolvePortableCommandSpec } from '../policy.js';
import { parseWslUncPath } from '../wsl.js';
import { boundedInteger, commandTimeoutMaxMs } from './timeoutPolicy.js';
import { commandEnvironment, explicitEnvironment, removedEnvironment } from './environment.js';
import { capTail, terminationReason } from './output.js';
import {
  normalizedSandboxConfig,
  prepareSandboxLaunch,
  sandboxUsesPortableCommand,
  type SandboxLaunch
} from '../sandbox.js';

interface BufferedRunRouting {
  routeWsl?: boolean;
  environment?: Array<[string, string]>;
  removeEnvironment?: string[];
  wrappedProcess?: boolean;
  backendKill?: () => Promise<void>;
  onSpawn?: (pid: number | undefined) => void;
}

export interface ProcessPostCheckDependencies {
  runBuffered(
    program: string,
    args: string[],
    cwd: string,
    input?: string,
    timeoutMs?: number,
    environment?: NodeJS.ProcessEnv,
    routing?: BufferedRunRouting
  ): Promise<{ code: number | null; stdout: string; stderr: string }>;
  finalizeSession(ctx: ToolContext, session: ProcessSession, verificationOk: boolean): Promise<void>;
}

export async function runProcessPostChecks(
  deps: ProcessPostCheckDependencies,
  ctx: ToolContext,
  key: string,
  session: ProcessSession,
  checks: JsonObject[],
  cwd: string
): Promise<void> {
  if (!checks.length || session.exitCode !== 0 || terminationReason(session) !== 'exited') {
    await deps.finalizeSession(ctx, session, session.exitCode === 0 && terminationReason(session) === 'exited');
    return;
  }
  session.postChecksPending = true;
  session.events.emit('change');
  let verificationOk = true;
  const { root } = rootAndCwd(ctx, key);
  const sandboxed = Boolean(session.sandboxEnforced && session.sandboxBackend);
  const portableSandbox = sandboxUsesPortableCommand(session.sandboxBackend);
  for (let index = 0; index < checks.length; index += 1) {
    const check = checks[index];
    const checkArgs = { ...check, workdir: relativeInside(root, cwd) };
    const spec = portableSandbox
      ? await resolvePortableCommandSpec(ctx, key, checkArgs)
      : await resolveCommandSpec(ctx, key, checkArgs);
    const expected = Number(check.expected_exit_code ?? 0);
    const timeoutMs = boundedInteger(check.timeout_ms, 30_000, 1, commandTimeoutMaxMs(ctx));
    const maxOutput = boundedInteger(check.max_output_bytes, 16_384, 1, 1_048_576);
    const wslWorkspace = !sandboxed && Boolean(parseWslUncPath(cwd));
    let launch: SandboxLaunch | undefined;
    let result: { code: number | null; stdout: string; stderr: string };
    try {
      if (sandboxed) {
        launch = await prepareSandboxLaunch(
          normalizedSandboxConfig(ctx.config.sandbox),
          root,
          ctx.config.dataDir,
          cwd,
          spec,
          explicitEnvironment(check),
          removedEnvironment(check),
          undefined,
          timeoutMs
        );
      }
      result = await deps.runBuffered(
        launch?.program ?? spec.program,
        launch?.args ?? spec.argv,
        cwd,
        undefined,
        timeoutMs,
        launch?.environmentMode === 'forwarded' || wslWorkspace ? process.env : commandEnvironment(check),
        launch
          ? { wrappedProcess: true, backendKill: launch.kill, onSpawn: launch.onSpawn }
          : { routeWsl: true, environment: explicitEnvironment(check), removeEnvironment: removedEnvironment(check) }
      );
    } finally {
      if (launch) await launch.cleanup();
    }
    const passed = result.code === expected;
    verificationOk &&= passed;
    session.postChecks.push({
      index,
      name: String(check.name ?? `post-check-${index + 1}`),
      command: spec.display,
      expected_exit_code: expected,
      exit_code: result.code,
      ok: passed,
      stdout: capTail(result.stdout, maxOutput).content,
      stderr: capTail(result.stderr, maxOutput).content
    });
  }
  await deps.finalizeSession(ctx, session, verificationOk);
}
