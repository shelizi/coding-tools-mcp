import { createHash } from 'node:crypto';
import type { JsonObject } from './types.js';
import { redactSensitiveText } from './redaction.js';
import type { CheckpointRecord } from './historyModel.js';
import { HistoryError } from './historyModel.js';

export const CHECKPOINT_HEADING = '## 本轮检查点';
export const INHERITED_SUMMARY_HEADING = '继承的历史摘要';

const SUMMARY_SECTIONS = [
  '用户核心目标', '已确认事实', '已完成修改', '关键设计决定',
  '测试结果', '当前运行状态', '剩余问题', '下一步'
] as const;

export function metadata(content: string, label: string): string | undefined {
  const prefix = `**${label}:**`;
  for (const line of content.split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed.startsWith(prefix)) continue;
    const value = trimmed.slice(prefix.length).trim();
    if (value) return value;
  }
  return undefined;
}

export function documentTitle(content: string, number: number): string {
  const heading = content.split(/\r?\n/).map(line => line.trim()).find(line => line.startsWith('# '));
  const title = heading?.slice(2).split('：', 2)[1]?.trim() || '开发会话';
  return title.replace(`会话 ${number}`, '').replace(/^[：: ]+|[：: ]+$/g, '') || '开发会话';
}

function uniqueValues(values: Iterable<string>): string[] {
  const output: string[] = [];
  for (const raw of values) {
    const value = raw.trim();
    if (value && !output.includes(value)) output.push(value);
  }
  return output;
}

function renderSection(heading: string, values: Iterable<string>): string {
  const rows = uniqueValues(values).map(value => `- ${value}`).join('\n');
  return `## ${heading}\n\n${rows}${rows ? '\n' : ''}\n`;
}

export function renderDocument(
  number: number,
  title: string,
  sessionKey: string,
  createdAt: string,
  updatedAt: string,
  status: string,
  records: readonly CheckpointRecord[]
): string {
  const safeTitle = title.trim() || '开发会话';
  let output = `# 会话 ${number}：${safeTitle}\n\n`
    + `**Session key:** ${sessionKey}\n`
    + `**Created:** ${createdAt}\n`
    + `**Updated:** ${updatedAt}\n`
    + `**Status:** ${status}\n\n`;
  output += renderSection('用户核心目标', records.map(record => record.user_intent));
  output += renderSection('已确认事实', records.flatMap(record => record.findings));
  output += renderSection('已完成修改', records.flatMap(record => record.files_changed));
  output += renderSection('关键设计决定', records.flatMap(record => record.decisions));
  output += renderSection('测试结果', records.flatMap(record => record.tests));
  output += renderSection('当前运行状态', records.flatMap(record => record.runtime_state));
  output += renderSection('剩余问题', records.flatMap(record => record.remaining_issues));
  output += renderSection('下一步', records.flatMap(record => record.next_actions));
  output += `${CHECKPOINT_HEADING}\n\n`;
  for (const record of records) {
    output += `### ${record.turn_id}\n\n\`\`\`json\n${JSON.stringify(record, null, 2)}\n\`\`\`\n\n`;
  }
  return output;
}

export function attachInheritedSummary(content: string, summary: string): string {
  const value = summary.trim();
  if (!value) return content;
  const statusStart = content.indexOf('**Status:**');
  if (statusStart < 0) return content;
  const relativeEnd = content.slice(statusStart).indexOf('\n\n');
  if (relativeEnd < 0) return content;
  const insertAt = statusStart + relativeEnd + 2;
  return content.slice(0, insertAt)
    + `## ${INHERITED_SUMMARY_HEADING}\n\n${value}\n\n`
    + content.slice(insertAt);
}

function sectionBody(content: string, heading: string): string | undefined {
  const marker = `## ${heading}`;
  const start = content.indexOf(marker);
  if (start < 0) return undefined;
  const tail = content.slice(start + marker.length);
  const end = tail.indexOf('\n## ');
  return tail.slice(0, end < 0 ? undefined : end).trim();
}

export function inheritedSummary(content: string): string | undefined {
  const value = sectionBody(content, INHERITED_SUMMARY_HEADING)?.trim();
  return value || undefined;
}

function validCheckpointRecord(value: unknown): value is CheckpointRecord {
  if (!value || typeof value !== 'object' || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  const stringFields = ['turn_id', 'timestamp', 'user_intent', 'notes'];
  const arrayFields = ['findings', 'decisions', 'files_changed', 'tests', 'runtime_state', 'remaining_issues', 'next_actions'];
  return stringFields.every(field => typeof record[field] === 'string')
    && arrayFields.every(field => Array.isArray(record[field]) && (record[field] as unknown[]).every(item => typeof item === 'string'));
}

export function parseCheckpointRecords(content: string): CheckpointRecord[] {
  const checkpoint = content.indexOf(CHECKPOINT_HEADING);
  if (checkpoint < 0) return [];
  const records: CheckpointRecord[] = [];
  const regex = /(?:^|\r?\n)### [^\r\n]+\r?\n\r?\n```json\r?\n([\s\S]*?)\r?\n```/g;
  const tail = content.slice(checkpoint + CHECKPOINT_HEADING.length);
  for (const match of tail.matchAll(regex)) {
    try {
      const parsed = JSON.parse(match[1] ?? '');
      if (validCheckpointRecord(parsed)) records.push(parsed);
    } catch {
      // Rust skips malformed checkpoint blocks while preserving valid records.
    }
  }
  return records;
}

export function historySummary(content: string): string {
  const parts: string[] = [];
  for (const section of SUMMARY_SECTIONS) {
    const body = sectionBody(content, section);
    if (!body) continue;
    const compact = body.split(/\r?\n/).map(line => line.trim()).filter(Boolean).join(' ');
    if (compact) parts.push(`${section}: ${compact}`);
  }
  return parts.length ? parts.join('；') : '未记录结构化摘要';
}

function stringField(args: JsonObject, name: string): string | undefined {
  return typeof args[name] === 'string' ? String(args[name]) : undefined;
}

function stringArray(args: JsonObject, name: string): string[] {
  const value = args[name];
  if (value === undefined) return [];
  if (!Array.isArray(value) || !value.every(item => typeof item === 'string')) {
    throw new HistoryError('INVALID_ARGUMENT', `${name} must be an array of strings`, 'validation', false, { argument: name });
  }
  return [...value] as string[];
}

export function checkpointFromArgs(args: JsonObject, defaultTimestamp: string): { record: CheckpointRecord; timestampWasExplicit: boolean } {
  const explicitTurnId = typeof args.turn_id === 'string' && args.turn_id.trim() ? args.turn_id.trim() : '';
  const explicitTimestamp = stringField(args, 'timestamp');
  const timestampWasExplicit = explicitTimestamp?.trim().length ? true : false;
  const record: CheckpointRecord = {
    turn_id: explicitTurnId,
    timestamp: explicitTimestamp ?? '',
    user_intent: stringField(args, 'user_intent') ?? '',
    findings: stringArray(args, 'findings'),
    decisions: stringArray(args, 'decisions'),
    files_changed: stringArray(args, 'files_changed'),
    tests: stringArray(args, 'tests'),
    runtime_state: stringArray(args, 'runtime_state'),
    remaining_issues: stringArray(args, 'remaining_issues'),
    next_actions: stringArray(args, 'next_actions'),
    notes: stringField(args, 'notes') ?? ''
  };
  if (!record.turn_id) {
    const hash = createHash('sha256').update(JSON.stringify(record)).digest('hex');
    record.turn_id = `auto-${hash.slice(0, 16)}`;
  }
  record.timestamp = explicitTimestamp ?? defaultTimestamp;
  return { record, timestampWasExplicit };
}

export function redactCheckpointRecord(record: CheckpointRecord): boolean {
  let changed = false;
  const redact = (value: string): string => {
    const output = redactSensitiveText(value);
    if (output.count > 0) changed = true;
    return output.value;
  };
  record.timestamp = redact(record.timestamp);
  record.user_intent = redact(record.user_intent);
  record.notes = redact(record.notes);
  for (const field of ['findings', 'decisions', 'files_changed', 'tests', 'runtime_state', 'remaining_issues', 'next_actions'] as const) {
    record[field] = record[field].map(redact);
  }
  return changed;
}

export function truncateChars(value: string, maxChars: number): string {
  const chars = Array.from(value);
  if (chars.length <= maxChars) return value;
  return `${chars.slice(0, maxChars).join('')}…（摘要已截断）`;
}
