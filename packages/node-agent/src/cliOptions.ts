export const RESTART_SUPERVISED_FLAG = '--restart-supervised';

export function restartSupervisedFromArgv(argv: readonly string[]): boolean {
  return argv.includes(RESTART_SUPERVISED_FLAG);
}
