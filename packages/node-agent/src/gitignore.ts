import { lstat, readFile } from 'node:fs/promises';
import path from 'node:path';

const MAX_IGNORE_FILE_BYTES = 1024 * 1024;
const IGNORE_FILES = ['.gitignore', '.ignore'] as const;

export interface IgnoreRule {
  base: string;
  negated: boolean;
  directoryOnly: boolean;
  exact: RegExp;
  tree: RegExp;
}

function regexEscape(value: string): string {
  return value.replace(/[|\\{}()[\]^$+?.-]/g, '\\$&');
}

function isEscaped(value: string, index: number): boolean {
  let count = 0;
  for (let cursor = index - 1; cursor >= 0 && value[cursor] === '\\'; cursor -= 1) count += 1;
  return count % 2 === 1;
}

function trimTrailingSpaces(value: string): string {
  let end = value.length;
  while (end > 0 && value[end - 1] === ' ' && !isEscaped(value, end - 1)) end -= 1;
  return value.slice(0, end);
}

function characterClass(pattern: string, start: number): { source: string; end: number } | undefined {
  let end = start + 1;
  if (pattern[end] === '!' || pattern[end] === '^') end += 1;
  if (pattern[end] === ']') end += 1;
  while (end < pattern.length && pattern[end] !== ']') end += 1;
  if (end >= pattern.length) return undefined;
  let content = pattern.slice(start + 1, end);
  let negated = false;
  if (content.startsWith('!') || content.startsWith('^')) {
    negated = true;
    content = content.slice(1);
  }
  content = content.replaceAll('\\', '\\\\').replaceAll(']', '\\]');
  return { source: `[${negated ? '^' : ''}${content}]`, end };
}

function globSource(pattern: string): string {
  let source = '';
  for (let index = 0; index < pattern.length; index += 1) {
    const char = pattern[index];
    if (char === '\\' && index + 1 < pattern.length) {
      source += regexEscape(pattern[index + 1]);
      index += 1;
      continue;
    }
    if (char === '*') {
      if (pattern[index + 1] === '*') {
        while (pattern[index + 1] === '*') index += 1;
        if (pattern[index + 1] === '/') {
          source += '(?:.*/)?';
          index += 1;
        } else {
          source += '.*';
        }
      } else {
        source += '[^/]*';
      }
      continue;
    }
    if (char === '?') {
      source += '[^/]';
      continue;
    }
    if (char === '[') {
      const parsed = characterClass(pattern, index);
      if (parsed) {
        source += parsed.source;
        index = parsed.end;
        continue;
      }
    }
    source += regexEscape(char);
  }
  return source;
}

function parseRule(lineValue: string, base: string): IgnoreRule | undefined {
  let line = trimTrailingSpaces(lineValue.replace(/\r$/, ''));
  if (!line || line[0] === '#') return undefined;
  let negated = false;
  if (line[0] === '!') {
    negated = true;
    line = line.slice(1);
  }
  if (!line) return undefined;
  let directoryOnly = false;
  if (line.endsWith('/') && !isEscaped(line, line.length - 1)) {
    directoryOnly = true;
    line = line.slice(0, -1);
  }
  const anchored = line.startsWith('/');
  if (anchored) line = line.slice(1);
  if (!line) return undefined;
  const source = globSource(line);
  const pathRelative = anchored || line.includes('/');
  const exactSource = pathRelative ? `^${source}$` : `(?:^|/)${source}$`;
  const treeSource = pathRelative ? `^${source}(?:/.*)?$` : `(?:^|/)${source}(?:/.*)?$`;
  return {
    base,
    negated,
    directoryOnly,
    exact: new RegExp(exactSource),
    tree: new RegExp(treeSource)
  };
}

export function parseIgnoreFile(content: string, base: string): IgnoreRule[] {
  const normalizedBase = base.replaceAll('\\', '/').replace(/^\.\/?$/, '').replace(/\/$/, '');
  const rules: IgnoreRule[] = [];
  for (const line of content.replace(/^\uFEFF/, '').split('\n')) {
    const rule = parseRule(line, normalizedBase);
    if (rule) rules.push(rule);
  }
  return rules;
}

function localPath(rule: IgnoreRule, relative: string): string | undefined {
  if (!rule.base) return relative;
  if (!relative.startsWith(`${rule.base}/`)) return undefined;
  return relative.slice(rule.base.length + 1);
}

export function isIgnoredByRules(relativeValue: string, isDirectory: boolean, rules: readonly IgnoreRule[]): boolean {
  const relative = relativeValue.replaceAll('\\', '/').replace(/^\.\//, '');
  let ignored = false;
  for (const rule of rules) {
    const local = localPath(rule, relative);
    if (!local || !rule.tree.test(local)) continue;
    if (rule.directoryOnly && rule.exact.test(local) && !isDirectory) continue;
    ignored = !rule.negated;
  }
  return ignored;
}

async function rulesFromFile(file: string, base: string): Promise<IgnoreRule[]> {
  try {
    const info = await lstat(file);
    if (!info.isFile() || info.size > MAX_IGNORE_FILE_BYTES) return [];
    return parseIgnoreFile((await readFile(file)).toString('utf8'), base);
  } catch {
    return [];
  }
}

export async function rootIgnoreRules(root: string): Promise<IgnoreRule[]> {
  const rules: IgnoreRule[] = [];
  const gitDirectory = path.join(root, '.git');
  try {
    if ((await lstat(gitDirectory)).isDirectory()) {
      rules.push(...await rulesFromFile(path.join(gitDirectory, 'info', 'exclude'), ''));
    }
  } catch { /* no repository-local exclude file */ }
  for (const name of IGNORE_FILES) rules.push(...await rulesFromFile(path.join(root, name), ''));
  return rules;
}

export async function extendIgnoreRules(root: string, directory: string, inherited: readonly IgnoreRule[]): Promise<IgnoreRule[]> {
  const base = path.relative(root, directory).replaceAll('\\', '/');
  const appended: IgnoreRule[] = [];
  for (const name of IGNORE_FILES) appended.push(...await rulesFromFile(path.join(directory, name), base));
  return appended.length ? [...inherited, ...appended] : [...inherited];
}
