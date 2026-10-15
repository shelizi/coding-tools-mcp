import { createHash } from 'node:crypto';
import type { HistoryDocument } from './historyModel.js';
import { documentTitle, historySectionItems, historySummary } from './historyMarkdown.js';

export interface HistoryRetrievalDocument {
  sessionNumber: number;
  path: string;
  updatedAt: string;
  title: string;
  userIntent: string[];
  findings: string[];
  decisions: string[];
  remainingIssues: string[];
  nextActions: string[];
  filesChanged: string[];
  tests: string[];
  summary: string;
}

export interface HistoryRetrievalIndex {
  version: 1;
  sourceDigest: string;
  documentCount: number;
  documents: HistoryRetrievalDocument[];
}

export interface HistoryRetrievalHit {
  sessionNumber: number;
  path: string;
  updatedAt: string;
  title: string;
  score: number;
  matchedFields: string[];
  summary: string;
}

const FIELD_WEIGHTS: ReadonlyArray<[keyof Pick<HistoryRetrievalDocument,
  'title' | 'userIntent' | 'findings' | 'decisions' | 'remainingIssues' | 'nextActions' | 'filesChanged' | 'tests' | 'summary'>, number]> = [
  ['title', 5.5],
  ['userIntent', 5],
  ['decisions', 4.5],
  ['remainingIssues', 4.25],
  ['filesChanged', 4],
  ['nextActions', 3.5],
  ['findings', 3],
  ['tests', 2.25],
  ['summary', 1]
];

function unique(values: Iterable<string>): string[] {
  return [...new Set([...values].map(value => value.trim()).filter(Boolean))];
}

function normalized(value: string): string {
  return value.normalize('NFKC').toLocaleLowerCase('en-US').replace(/\s+/g, ' ').trim();
}

function cjkGrams(value: string): string[] {
  const output: string[] = [];
  for (const match of value.matchAll(/[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}]+/gu)) {
    const chars = Array.from(match[0]);
    if (chars.length <= 8) output.push(chars.join(''));
    for (const size of [2, 3]) {
      for (let index = 0; index + size <= chars.length; index += 1) output.push(chars.slice(index, index + size).join(''));
    }
  }
  return output;
}

export function historyRetrievalTokens(value: string): string[] {
  const text = normalized(value);
  if (!text) return [];
  const identifiers = text.match(/[a-z0-9]+(?:[._/@:+-][a-z0-9]+)*/g) ?? [];
  const identifierParts = identifiers.flatMap(identifier => identifier.split(/[._/@:+-]+/g).filter(part => part.length >= 2));
  const unicodeWords = text.match(/[\p{L}\p{N}]{2,}/gu) ?? [];
  return unique([...identifiers, ...identifierParts, ...unicodeWords, ...cjkGrams(text)]);
}

function fieldText(value: string | string[]): string {
  return Array.isArray(value) ? value.join('\n') : value;
}

function sourceDigest(documents: readonly HistoryRetrievalDocument[]): string {
  const hash = createHash('sha256');
  for (const document of documents) {
    hash.update(JSON.stringify(document));
    hash.update('\n');
  }
  return hash.digest('hex');
}

export function historyRetrievalDocument(document: HistoryDocument): HistoryRetrievalDocument {
  return {
    sessionNumber: document.number,
    path: document.path,
    updatedAt: document.updated_at ?? '',
    title: documentTitle(document.content, document.number),
    userIntent: historySectionItems(document.content, '用户核心目标'),
    findings: historySectionItems(document.content, '已确认事实'),
    decisions: historySectionItems(document.content, '关键设计决定'),
    remainingIssues: historySectionItems(document.content, '剩余问题'),
    nextActions: historySectionItems(document.content, '下一步'),
    filesChanged: historySectionItems(document.content, '已完成修改'),
    tests: historySectionItems(document.content, '测试结果'),
    summary: historySummary(document.content)
  };
}

export function buildHistoryRetrievalIndex(documents: readonly HistoryDocument[]): HistoryRetrievalIndex {
  const structured = [...documents]
    .sort((left, right) => left.number - right.number)
    .map(historyRetrievalDocument);
  return {
    version: 1,
    sourceDigest: sourceDigest(structured),
    documentCount: structured.length,
    documents: structured
  };
}

function documentFrequency(index: HistoryRetrievalIndex): Map<string, number> {
  const frequency = new Map<string, number>();
  for (const document of index.documents) {
    const tokens = new Set<string>();
    for (const [field] of FIELD_WEIGHTS) for (const token of historyRetrievalTokens(fieldText(document[field]))) tokens.add(token);
    for (const token of tokens) frequency.set(token, (frequency.get(token) ?? 0) + 1);
  }
  return frequency;
}

function idf(documentCount: number, frequency: number): number {
  return Math.log((documentCount + 1) / (frequency + 1)) + 1;
}

export function searchHistoryRetrievalIndex(
  index: HistoryRetrievalIndex,
  query: string,
  options: { topK?: number; recentBoost?: number } = {}
): HistoryRetrievalHit[] {
  const queryText = normalized(query);
  const queryTokens = historyRetrievalTokens(query);
  if (!queryText || !queryTokens.length) return [];
  const topK = Math.max(1, Math.min(20, Math.trunc(options.topK ?? 8) || 8));
  const recentBoost = Math.max(0, Math.min(1, Number(options.recentBoost ?? 0.05) || 0));
  const frequency = documentFrequency(index);
  const maxNumber = Math.max(1, ...index.documents.map(document => document.sessionNumber));
  const hits: HistoryRetrievalHit[] = [];

  for (const document of index.documents) {
    let score = 0;
    const matchedFields: string[] = [];
    for (const [field, weight] of FIELD_WEIGHTS) {
      const raw = fieldText(document[field]);
      const text = normalized(raw);
      if (!text) continue;
      const tokens = new Set(historyRetrievalTokens(raw));
      let fieldScore = 0;
      for (const token of queryTokens) {
        if (tokens.has(token)) fieldScore += weight * idf(index.documentCount, frequency.get(token) ?? 0);
      }
      if (queryText.length >= 2 && text.includes(queryText)) fieldScore += weight * 4;
      if (field === 'filesChanged' && /[./\\_-]/.test(queryText) && text.includes(queryText)) fieldScore += weight * 2;
      if (fieldScore > 0) {
        score += fieldScore;
        matchedFields.push(field);
      }
    }
    if (score <= 0) continue;
    score += recentBoost * (document.sessionNumber / maxNumber);
    hits.push({
      sessionNumber: document.sessionNumber,
      path: document.path,
      updatedAt: document.updatedAt,
      title: document.title,
      score: Number(score.toFixed(6)),
      matchedFields,
      summary: document.summary
    });
  }

  return hits
    .sort((left, right) => right.score - left.score || right.sessionNumber - left.sessionNumber || left.path.localeCompare(right.path))
    .slice(0, topK);
}
