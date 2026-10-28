import { containsSensitivePath, redactSensitiveText } from './redaction.js';

export const AUTO_ACTIVITY_HEADING = '## 自動擷取的工具活動（非工作結論）';
export const AUTO_ACTIVITY_START = '<!-- coding-tools-mcp:auto-activity:start -->';
export const AUTO_ACTIVITY_END = '<!-- coding-tools-mcp:auto-activity:end -->';
const CHECKPOINT_HEADING = '## 本轮检查点';
export const MAX_AUTO_ACTIVITY_LINES = 48;
export const MAX_AUTO_ACTIVITY_BYTES = 14_000;

export interface AutoActivityEvent {
  id: string;
  atMs: number;
  tool: string;
  action: string;
  status: string;
}

interface ExistingBlock { start: number; end: number; rows: string[]; }

function findBlock(content: string): ExistingBlock | undefined {
  const start = content.indexOf(AUTO_ACTIVITY_HEADING);
  if (start < 0) return undefined;
  const begin = content.indexOf(AUTO_ACTIVITY_START, start);
  const endMarker = content.indexOf(AUTO_ACTIVITY_END, begin < 0 ? start : begin);
  const checkpoint = content.indexOf(CHECKPOINT_HEADING);
  if (begin < 0 || endMarker < 0 || (checkpoint >= 0 && endMarker > checkpoint)) {
    throw new Error('Invalid automatic activity section; refusing to overwrite history');
  }
  let end = endMarker + AUTO_ACTIVITY_END.length;
  while (content[end] === '\n' || content[end] === '\r') end++;
  const existing = content.slice(begin + AUTO_ACTIVITY_START.length, endMarker);
  const rows = existing.split(/\r?\n/).filter(line =>
    /^- .{1,768} <!-- ctmcp-activity:[A-Za-z0-9_.:-]{1,100} -->$/.test(line));
  return { start, end, rows };
}

function safeText(value: string, limit: number): string {
  if (containsSensitivePath(value)) return '[sensitive path]';
  const redacted = redactSensitiveText(value).value;
  return Array.from(redacted.replace(/[\r\n\t<>\x00-\x1f`*|\\#\[\]{}]/g, ' ').replace(/\s+/g, ' ').trim())
    .slice(0, limit).join('');
}

function eventLine(event: AutoActivityEvent): string | undefined {
  if (!/^[A-Za-z0-9_.:-]{1,100}$/.test(event.id) || !Number.isFinite(event.atMs)) return undefined;
  const tool = safeText(event.tool, 64);
  const action = safeText(event.action, 120);
  const status = ({
    running: '呼叫中', success: '請求完成', failed: '請求失敗', rpc_error: '通訊失敗',
    tool_error: '工具回應錯誤', cancelled: '已取消', worker_failed: '工作程序失敗',
    background_running: '背景程序執行中', background_ended: '背景程序已結束',
  } as Record<string, string>)[event.status] ?? '狀態已更新';
  if (!tool) return undefined;
  return `- ${new Date(Math.max(0, event.atMs)).toISOString()} · ${status} · ${tool}${action && action !== tool ? ` · ${action}` : ''} <!-- ctmcp-activity:${event.id} -->`;
}

function activitySection(rows: string[]): string {
  return AUTO_ACTIVITY_HEADING
    + '\n\n> Coding MCP 自動觀測的工具呼叫紀錄，並非 Agent 確認的結論。只保留最近紀錄，不包含命令輸出或完整參數。\n\n'
    + AUTO_ACTIVITY_START + '\n'
    + rows.join('\n') + '\n'
    + AUTO_ACTIVITY_END + '\n\n';
}

/** Merge events under the same history lock used by checkpoints. Only the
 * bounded generated section changes; never touch JSON checkpoint records. */
export function mergeAutoActivity(content: string, events: readonly AutoActivityEvent[]): string {
  if (!events.length) return content;
  const block = findBlock(content);
  const rows = block?.rows ?? [];
  const known = new Set(rows.map(row => row.match(/<!-- ctmcp-activity:([^ ]+) -->$/)?.[1]).filter(Boolean));
  for (const event of [...events].sort((left, right) => left.atMs - right.atMs || left.id.localeCompare(right.id))) {
    const row = eventLine(event);
    if (!row || known.has(event.id)) continue;
    rows.push(row);
    known.add(event.id);
  }
  const bounded: string[] = [];
  let bytes = 0;
  for (const row of rows.slice().reverse()) {
    const size = Buffer.byteLength(row, 'utf8') + 1;
    if (bounded.length >= MAX_AUTO_ACTIVITY_LINES || bytes + size > MAX_AUTO_ACTIVITY_BYTES) break;
    bounded.unshift(row);
    bytes += size;
  }
  const section = activitySection(bounded);
  if (block) return content.slice(0, block.start) + section + content.slice(block.end);
  const checkpoint = content.indexOf(CHECKPOINT_HEADING);
  if (checkpoint < 0) throw new Error('History checkpoint section missing; refusing to overwrite');
  return content.slice(0, checkpoint) + section + content.slice(checkpoint);
}

/** Re-rendering a semantic checkpoint must retain the generated activity log. */
export function preserveAutoActivity(rendered: string, previous: string): string {
  const block = findBlock(previous);
  if (!block) return rendered;
  const checkpoint = rendered.indexOf(CHECKPOINT_HEADING);
  if (checkpoint < 0) throw new Error('New checkpoint section missing');
  return rendered.slice(0, checkpoint) + previous.slice(block.start, block.end)
    + rendered.slice(checkpoint);
}

export interface PersistedActivityObservation {
  sequence: number;
  tool: string;
  action: string;
  startedAtMs: number;
  updatedAtMs: number;
  /** Past in-flight operations are observations, not proof they are still running. */
  status: 'success' | 'failed' | 'cancelled' | 'observed';
}

/**
 * Recover only actual, bounded tool events written by Coding MCP, not a
 * checkpoint's Updated timestamp (which is not proof of runtime activity).
 * The Node writer uses ISO timestamps; Rust writes unix:seconds. Ignore any
 * future events beyond a small clock-skew allowance and never revive a job
 * as 'running' based on a previous process's Markdown.
 */
export function recentPersistedActivity(
  content: string,
  nowMs = Date.now(),
  windowMs = 5 * 60_000,
): PersistedActivityObservation[] {
  let rows: string[];
  try {
    rows = findBlock(content)?.rows ?? [];
  } catch {
    // A damaged editable Markdown section must not break the History page.
    return [];
  }
  const recent = new Map<string, PersistedActivityObservation>();
  for (const [rowIndex, line] of rows.slice(-MAX_AUTO_ACTIVITY_LINES).entries()) {
    const match = line.match(
      /^- (\S{8,35}) · [^·\r\n]{1,48} · (.{1,350}) <!-- ctmcp-activity:((?:node|rust):[A-Za-z0-9_.:-]{1,96}) -->$/,
    );
    if (!match) continue;
    const [, when, description, id] = match;
    const atMs = when.startsWith('unix:')
      ? Number(when.slice(5)) * 1_000
      : Date.parse(when);
    if (!Number.isFinite(atMs) || atMs <= 0 || atMs > nowMs + 30_000
      || nowMs - atMs > windowMs) continue;
    const statusTag = id.split(':').at(-1);
    const status: PersistedActivityObservation['status'] =
      statusTag === 'success' ? 'success'
        : ['failed', 'rpc_error', 'tool_error', 'worker_failed'].includes(statusTag ?? '') ? 'failed'
          : statusTag === 'cancelled' ? 'cancelled' : 'observed';
    const tool = description.split(' · ', 1)[0]?.trim() ?? '';
    if (!/^[A-Za-z0-9_.-]{1,64}$/.test(tool)) continue;
    // The persisted row prefixes the tool name, and the recorder's action
    // summary often already prefixes it too. Do not show "read_file ·
    // read_file · path" after recovery from a service restart.
    const once = description.slice(tool.length + 3);
    const action = once.startsWith(`${tool} · `) ? once : description;
    const event: PersistedActivityObservation = {
      // The row index is unique within this session; sequence counters can
      // collide across agent restarts, and background IDs have no counter.
      sequence: rowIndex + 1,
      tool,
      action,
      startedAtMs: Math.min(atMs, nowMs),
      updatedAtMs: Math.min(atMs, nowMs),
      status,
    };
    const eventKey = id.replace(/:(?:running|success|failed|cancelled|rpc_error|tool_error|worker_failed|background_running|background_ended)$/, '');
    const old = recent.get(eventKey);
    if (!old || old.updatedAtMs <= event.updatedAtMs) recent.set(eventKey, event);
  }
  return [...recent.values()]
    .sort((a, b) => b.updatedAtMs - a.updatedAtMs || b.sequence - a.sequence)
    .slice(0, 16);
}
