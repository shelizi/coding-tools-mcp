import { createHash } from 'node:crypto';
import { appendFile, mkdir, readFile, rename, rm, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import type { ExperienceObservation } from './types.js';

export interface ExperienceLedgerRecord {
  schemaVersion: 1;
  eventId: string;
  tool: string;
  outcome: 'success' | 'failure';
  durationMs: number;
  requestBytes?: number;
  responseBytes?: number;
  errorCode?: string;
  features?: Record<string, string | number | boolean | null>;
  verificationOutcome?: 'passed' | 'failed';
  recoveryOutcome?: 'passed' | 'failed';
  taskIdHash?: string;
  conversationContextIdHash?: string;
  runtimeBootIdHash?: string;
  timestampMs: number;
}

export interface ExperienceLedgerState {
  schemaVersion: 1;
  compilerRevision: string;
  normalizerRevision: string;
  lastCanonicalSourceRevision?: string;
  lastReplayAtMs: number;
  ledgerEvents: number;
  materializedEvents: number;
  ignoredEvents: number;
  replayComplete: boolean;
}

const LEDGER_FILE = 'experience.jsonl';
const STATE_FILE = 'ingestion-state.json';
const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const ledgerRootTails = new Map<string, Promise<void>>();
let stateTempSequence = 0;

async function withLedgerRootLock<T>(rootDir: string, operation: () => Promise<T>): Promise<T> {
  const key = path.resolve(rootDir);
  const previous = ledgerRootTails.get(key) ?? Promise.resolve();
  let release!: () => void;
  const current = new Promise<void>(resolve => { release = resolve; });
  const tail = previous.then(() => current);
  ledgerRootTails.set(key, tail);
  await previous;
  try {
    return await operation();
  } finally {
    release();
    void tail.finally(() => {
      if (ledgerRootTails.get(key) === tail) ledgerRootTails.delete(key);
    });
  }
}

function digest(kind: string, value: string | undefined): string | undefined {
  const normalized = value?.trim();
  return normalized ? createHash('sha256').update(`${kind}\0${normalized}`).digest('hex') : undefined;
}

function normalizedHash(kind: string, hash: string | undefined, raw: string | undefined): string | undefined {
  if (hash && SHA256_PATTERN.test(hash)) return hash;
  return digest(kind, raw);
}

function ledgerRecord(observation: ExperienceObservation): ExperienceLedgerRecord {
  return {
    schemaVersion: 1,
    eventId: observation.eventId,
    tool: observation.tool,
    outcome: observation.outcome,
    durationMs: Math.max(0, observation.durationMs),
    ...(observation.requestBytes === undefined ? {} : { requestBytes: Math.max(0, observation.requestBytes) }),
    ...(observation.responseBytes === undefined ? {} : { responseBytes: Math.max(0, observation.responseBytes) }),
    ...(observation.errorCode ? { errorCode: observation.errorCode } : {}),
    ...(observation.features ? { features: structuredClone(observation.features) } : {}),
    ...(observation.verificationOutcome ? { verificationOutcome: observation.verificationOutcome } : {}),
    ...(observation.recoveryOutcome ? { recoveryOutcome: observation.recoveryOutcome } : {}),
    ...(normalizedHash('task', observation.taskIdHash, observation.taskId) ? { taskIdHash: normalizedHash('task', observation.taskIdHash, observation.taskId) } : {}),
    ...(normalizedHash('conversation', observation.conversationContextIdHash, observation.conversationContextId) ? { conversationContextIdHash: normalizedHash('conversation', observation.conversationContextIdHash, observation.conversationContextId) } : {}),
    ...(normalizedHash('runtime-boot', observation.runtimeBootIdHash, observation.runtimeBootId) ? { runtimeBootIdHash: normalizedHash('runtime-boot', observation.runtimeBootIdHash, observation.runtimeBootId) } : {}),
    timestampMs: Math.max(0, observation.timestampMs)
  };
}

export function experienceLedgerRecordToObservation(record: ExperienceLedgerRecord): ExperienceObservation {
  return {
    eventId: record.eventId,
    tool: record.tool,
    outcome: record.outcome,
    durationMs: record.durationMs,
    ...(record.requestBytes === undefined ? {} : { requestBytes: record.requestBytes }),
    ...(record.responseBytes === undefined ? {} : { responseBytes: record.responseBytes }),
    ...(record.errorCode ? { errorCode: record.errorCode } : {}),
    ...(record.features ? { features: structuredClone(record.features) } : {}),
    ...(record.verificationOutcome ? { verificationOutcome: record.verificationOutcome } : {}),
    ...(record.recoveryOutcome ? { recoveryOutcome: record.recoveryOutcome } : {}),
    ...(record.taskIdHash ? { taskIdHash: record.taskIdHash } : {}),
    ...(record.conversationContextIdHash ? { conversationContextIdHash: record.conversationContextIdHash } : {}),
    ...(record.runtimeBootIdHash ? { runtimeBootIdHash: record.runtimeBootIdHash } : {}),
    timestampMs: record.timestampMs
  };
}

export class ExperienceLedger {
  private readonly ledgerPath: string;
  private readonly statePath: string;
  private loaded = false;
  private ids = new Set<string>();
  private records: ExperienceLedgerRecord[] = [];
  private ledgerBytes = 0;
  private state: ExperienceLedgerState;

  constructor(
    readonly rootDir: string,
    revisions: { compilerRevision: string; normalizerRevision: string }
  ) {
    this.ledgerPath = path.join(rootDir, LEDGER_FILE);
    this.statePath = path.join(rootDir, STATE_FILE);
    this.state = {
      schemaVersion: 1,
      compilerRevision: revisions.compilerRevision,
      normalizerRevision: revisions.normalizerRevision,
      lastReplayAtMs: 0,
      ledgerEvents: 0,
      materializedEvents: 0,
      ignoredEvents: 0,
      replayComplete: false
    };
  }

  async append(observations: readonly ExperienceObservation[]): Promise<{ added: number; deduped: number; invalid: number }> {
    return withLedgerRootLock(this.rootDir, async () => {
      await this.loadUnlocked();
      await this.refreshUnlocked();
      const additions: ExperienceLedgerRecord[] = [];
      let deduped = 0;
      let invalid = 0;
      for (const observation of observations) {
        if (!observation.eventId || !observation.tool) { invalid += 1; continue; }
        if (this.ids.has(observation.eventId)) { deduped += 1; continue; }
        const record = ledgerRecord(observation);
        this.ids.add(record.eventId);
        this.records.push(record);
        additions.push(record);
      }
      if (additions.length) {
        await mkdir(this.rootDir, { recursive: true });
        const payload = additions.map(record => `${JSON.stringify(record)}\n`).join('');
        await appendFile(this.ledgerPath, payload, { encoding: 'utf8', mode: 0o600 });
        this.ledgerBytes += Buffer.byteLength(payload);
      }
      this.state.ledgerEvents = this.records.length;
      this.state.ignoredEvents += invalid;
      await this.writeStateUnlocked();
      return { added: additions.length, deduped, invalid };
    });
  }

  async pendingMaterialization(): Promise<ExperienceLedgerRecord[]> {
    return withLedgerRootLock(this.rootDir, async () => {
      await this.loadUnlocked();
      await this.refreshUnlocked();
      return this.records.slice(this.state.materializedEvents).map(record => structuredClone(record));
    });
  }

  async advanceMaterialized(ignoredEvents: number, count: number): Promise<void> {
    await withLedgerRootLock(this.rootDir, async () => {
      await this.loadUnlocked();
      await this.refreshUnlocked();
      this.state.materializedEvents = Math.min(this.records.length, this.state.materializedEvents + Math.max(0, count));
      this.state.ignoredEvents += Math.max(0, ignoredEvents);
      await this.writeStateUnlocked();
    });
  }

  async markReplay(sourceRevision: string | undefined, replayComplete: boolean, now = Date.now()): Promise<void> {
    await withLedgerRootLock(this.rootDir, async () => {
      await this.loadUnlocked();
      await this.refreshUnlocked();
      this.state.lastReplayAtMs = now;
      this.state.lastCanonicalSourceRevision = sourceRevision;
      this.state.replayComplete = replayComplete;
      await this.writeStateUnlocked();
    });
  }

  async snapshot(): Promise<ExperienceLedgerState> {
    return withLedgerRootLock(this.rootDir, async () => {
      await this.loadUnlocked();
      await this.refreshUnlocked();
      return structuredClone(this.state);
    });
  }

  private async loadUnlocked(): Promise<void> {
    if (this.loaded) return;
    this.loaded = true;
    await this.reloadLedgerUnlocked();
    await this.reloadStateUnlocked();
    await this.writeStateUnlocked();
  }

  private async refreshUnlocked(): Promise<void> {
    let bytes = 0;
    try {
      bytes = (await stat(this.ledgerPath)).size;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    if (bytes !== this.ledgerBytes) await this.reloadLedgerUnlocked();
    await this.reloadStateUnlocked();
  }

  private async reloadLedgerUnlocked(): Promise<void> {
    let content = '';
    try {
      content = await readFile(this.ledgerPath, 'utf8');
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    this.applyLedgerContent(content);
  }

  private applyLedgerContent(content: string): void {
    this.ids = new Set<string>();
    this.records = [];
    for (const line of content.split(/\r?\n/)) {
      if (!line) continue;
      try {
        const record = JSON.parse(line) as ExperienceLedgerRecord;
        if (record?.schemaVersion !== 1 || !record.eventId || !record.tool || this.ids.has(record.eventId)) continue;
        this.ids.add(record.eventId);
        this.records.push(record);
      } catch { /* Ignore only incomplete/invalid retained lines. */ }
    }
    this.ledgerBytes = Buffer.byteLength(content);
    this.state.ledgerEvents = this.records.length;
  }

  private async reloadStateUnlocked(): Promise<void> {
    let previous: ExperienceLedgerState | undefined;
    try {
      previous = JSON.parse(await readFile(this.statePath, 'utf8')) as ExperienceLedgerState;
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    if (previous?.schemaVersion !== 1) {
      this.state.ledgerEvents = this.records.length;
      return;
    }
    const revisionChanged = previous.compilerRevision !== this.state.compilerRevision || previous.normalizerRevision !== this.state.normalizerRevision;
    this.state = {
      ...previous,
      compilerRevision: this.state.compilerRevision,
      normalizerRevision: this.state.normalizerRevision,
      ledgerEvents: this.records.length,
      materializedEvents: revisionChanged ? 0 : Math.min(previous.materializedEvents, this.records.length),
      replayComplete: revisionChanged ? false : previous.replayComplete
    };
  }

  private async writeStateUnlocked(): Promise<void> {
    await mkdir(this.rootDir, { recursive: true });
    const sequence = stateTempSequence += 1;
    const temp = `${this.statePath}.${process.pid}.${sequence}.tmp`;
    try {
      await writeFile(temp, `${JSON.stringify(this.state, null, 2)}\n`, { encoding: 'utf8', mode: 0o600 });
      await rename(temp, this.statePath);
    } catch (error) {
      await rm(temp, { force: true }).catch(() => {});
      throw error;
    }
  }
}
