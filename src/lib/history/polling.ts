/**
 * Whole-project history scans read Markdown from disk, so avoid 3-second
 * polling for dozens of projects. The former 30-second global interval missed
 * almost every short running request; adapt to the number of configured folders.
 * A 3-second timer drives these intervals (6, 12 and 15 seconds respectively).
 */
export function historyPollCadenceMs(scope: string, folderCount: number): number {
  if (scope !== "all") return 3_000;
  if (folderCount <= 4) return 6_000;
  if (folderCount <= 8) return 12_000;
  return 15_000;
}
