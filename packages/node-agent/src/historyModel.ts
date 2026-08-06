import type { JsonObject } from './types.js';

export interface HistoryIndexEntry {
  number: number;
  path: string;
  created_at: string;
  updated_at: string;
}

export interface HistoryIndex {
  version: number;
  latest_number: number;
  sessions: Record<string, HistoryIndexEntry>;
}

export interface HistoryDocument {
  number: number;
  path: string;
  content: string;
  session_key?: string;
  created_at?: string;
  updated_at?: string;
}

export interface HistoryScanReport {
  documents: HistoryDocument[];
  numbers: number[];
  missing_numbers: number[];
  duplicate_session_keys: string[];
  invalid_files: string[];
  empty_files: string[];
}

export interface CheckpointRecord extends JsonObject {
  turn_id: string;
  timestamp: string;
  user_intent: string;
  findings: string[];
  decisions: string[];
  files_changed: string[];
  tests: string[];
  runtime_state: string[];
  remaining_issues: string[];
  next_actions: string[];
  notes: string;
}

export class HistoryError extends Error {
  readonly code: string;
  readonly category: string;
  readonly retryable: boolean;
  readonly details: JsonObject;

  constructor(code: string, message: string, category = 'validation', retryable = false, details: JsonObject = {}) {
    super(message);
    this.name = 'HistoryError';
    this.code = code;
    this.category = category;
    this.retryable = retryable;
    this.details = details;
  }
}

export function emptyHistoryIndex(): HistoryIndex {
  return { version: 1, latest_number: 0, sessions: {} };
}

export function latestHistoryNumber(report: HistoryScanReport): number | undefined {
  return report.numbers.at(-1);
}

export function historySequenceValid(report: HistoryScanReport): boolean {
  return report.missing_numbers.length === 0
    && report.duplicate_session_keys.length === 0
    && report.invalid_files.length === 0
    && report.empty_files.length === 0;
}
