export interface ParsedSkillMarkdown {
  name: string;
  description: string;
  body: string;
}

function unquote(value: string): string {
  const trimmed = value.trim();
  if (trimmed.startsWith('"') && trimmed.endsWith('"')) {
    try {
      return JSON.parse(trimmed) as string;
    } catch {
      throw new Error('invalid double-quoted YAML scalar');
    }
  }
  if (trimmed.startsWith("'") && trimmed.endsWith("'")) {
    return trimmed.slice(1, -1).replaceAll("''", "'");
  }
  return trimmed;
}

function parseFrontmatter(lines: string[]): Map<string, string> {
  const values = new Map<string, string>();
  for (let index = 0; index < lines.length;) {
    const line = lines[index]!;
    if (!line.trim() || line.trimStart().startsWith('#')) {
      index += 1;
      continue;
    }
    const match = /^([A-Za-z0-9_.-]+):(?:\s*(.*))?$/.exec(line);
    if (!match) {
      index += 1;
      continue;
    }
    const key = match[1]!.toLowerCase();
    const raw = match[2] ?? '';
    if (raw === '|' || raw === '>') {
      const block: string[] = [];
      index += 1;
      while (index < lines.length) {
        const nested = lines[index]!;
        if (/^[A-Za-z0-9_.-]+:/.test(nested)) break;
        if (nested.startsWith(' ') || nested.startsWith('\t') || !nested.trim()) {
          block.push(nested.replace(/^ {1,4}/, ''));
          index += 1;
          continue;
        }
        break;
      }
      values.set(key, raw === '>' ? block.join(' ').trim() : block.join('\n').trim());
      continue;
    }
    values.set(key, unquote(raw));
    index += 1;
  }
  return values;
}

export function parseSkillMarkdown(input: string): ParsedSkillMarkdown {
  const normalized = input.replace(/^\uFEFF/, '').replaceAll('\r\n', '\n');
  const lines = normalized.split('\n');
  if (lines[0]?.trim() !== '---') throw new Error('SKILL.md must start with YAML frontmatter');
  const closing = lines.slice(1, 257).findIndex(line => line.trim() === '---');
  if (closing < 0) throw new Error('SKILL.md frontmatter is not terminated');
  const end = closing + 1;
  const frontmatter = parseFrontmatter(lines.slice(1, end));
  const name = String(frontmatter.get('name') ?? '').trim();
  const description = String(frontmatter.get('description') ?? '').trim();
  if (!name) throw new Error('SKILL.md frontmatter requires name');
  if (!description) throw new Error('SKILL.md frontmatter requires description');
  if (name.length > 160) throw new Error('SKILL.md name is too long');
  if (description.length > 4_096) throw new Error('SKILL.md description is too long');
  const body = lines.slice(end + 1).join('\n').replace(/^\n+/, '');
  return { name, description, body };
}
