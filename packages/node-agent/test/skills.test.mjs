import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdir, mkdtemp, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { parseSkillMarkdown } from '../dist/skills/parser.js';
import { SkillRegistry } from '../dist/skills/registry.js';

async function writeSkill(root, relativeRoot, name, description, body = '# Skill') {
  const directory = path.join(root, relativeRoot);
  await mkdir(directory, { recursive: true });
  await writeFile(path.join(directory, 'SKILL.md'), `---\nname: ${name}\ndescription: ${JSON.stringify(description)}\n---\n\n${body}\n`);
}

test('SKILL.md parser accepts quoted metadata and requires name and description', () => {
  const parsed = parseSkillMarkdown('---\nname: demo\ndescription: "Quoted description"\n---\n\n# Demo\n');
  assert.equal(parsed.name, 'demo');
  assert.equal(parsed.description, 'Quoted description');
  assert.equal(parsed.body, '# Demo\n');
  assert.throws(() => parseSkillMarkdown('---\nname: demo\n---\n'), /requires description/);
});

test('project Skill registry discovers compatibility roots with deterministic precedence', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-skills-'));
  await writeSkill(root, path.join('.claude', 'skills', 'shared'), 'shared', 'Claude compatibility copy');
  await writeSkill(root, path.join('.agents', 'skills', 'shared'), 'shared', 'Agents compatibility copy');
  await writeSkill(root, path.join('skills', 'shared'), 'shared', 'Canonical project copy');
  await writeSkill(root, path.join('.claude', 'skills', 'gitnexus', 'debugging'), 'gitnexus-debugging', 'Debug with GitNexus');
  await writeFile(path.join(root, 'skills', 'shared', 'VERSION'), '2.3.4\n');

  const snapshot = await new SkillRegistry(root, { homeDir: null }).snapshot();
  assert.deepEqual(snapshot.skills.map(skill => skill.name), ['gitnexus-debugging', 'shared']);
  const shared = snapshot.skills.find(skill => skill.name === 'shared');
  assert.equal(shared.source, 'project');
  assert.equal(shared.scope, 'workspace');
  assert.equal(shared.description, 'Canonical project copy');
  assert.equal(shared.version, '2.3.4');
  assert.equal(snapshot.diagnostics.filter(item => item.code === 'SKILL_SHADOWED').length, 2);
  assert.match(snapshot.revision, /^[a-f0-9]{64}$/);
});

test('Skill registry discovers Codex and Claude Code user Skills and filters controls after shadowing', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-skills-workspace-'));
  const home = await mkdtemp(path.join(tmpdir(), 'ctmcp-skills-home-'));
  await writeSkill(home, path.join('.agents', 'skills', 'user-only'), 'user-only', 'Personal Codex workflow');
  await writeSkill(home, path.join('.agents', 'skills', 'shared'), 'shared', 'Personal shared workflow');
  await writeSkill(home, path.join('.claude', 'skills', 'claude-only'), 'claude-only', 'Personal Claude workflow');
  await writeSkill(root, path.join('skills', 'shared'), 'shared', 'Workspace shared workflow');

  const registry = new SkillRegistry(root, { homeDir: home, workspaceKey: 'repo' });
  const inventory = await registry.inventory();
  assert.deepEqual(inventory.skills.map(item => item.skill.name), ['claude-only', 'shared', 'user-only']);
  const userOnly = inventory.skills.find(item => item.skill.name === 'user-only').skill;
  assert.equal(userOnly.source, 'codex-user');
  assert.equal(userOnly.scope, 'user');
  assert.equal(userOnly.relativePath, '~/.agents/skills/user-only/SKILL.md');
  assert.equal(userOnly.relativePath.includes(home), false);
  const claudeOnly = inventory.skills.find(item => item.skill.name === 'claude-only').skill;
  assert.equal(claudeOnly.source, 'claude-user');
  assert.equal(claudeOnly.scope, 'user');
  assert.equal(claudeOnly.relativePath, '~/.claude/skills/claude-only/SKILL.md');
  assert.equal(claudeOnly.relativePath.includes(home), false);

  const sharedItem = inventory.skills.find(item => item.skill.name === 'shared');
  assert.equal(sharedItem.skill.source, 'project');
  assert.equal(sharedItem.skill.scope, 'workspace');
  assert.equal(sharedItem.skill.description, 'Workspace shared workflow');
  assert.match(sharedItem.skill.key, /^workspace:repo:project:/);
  const before = await registry.snapshot();
  registry.setDisabledSkillKeys([sharedItem.skill.key]);
  const disabledInventory = await registry.inventory();
  assert.equal(disabledInventory.skills.find(item => item.skill.name === 'shared').enabled, false);
  const after = await registry.snapshot();
  assert.deepEqual(after.skills.map(skill => skill.name), ['claude-only', 'user-only']);
  assert.equal(after.skills.some(skill => skill.name === 'shared'), false);
  assert.notEqual(after.revision, before.revision);

  registry.setActive(false);
  const masterDisabledInventory = await registry.inventory();
  assert.equal(masterDisabledInventory.skills.find(item => item.skill.name === 'user-only').selected, true);
  assert.equal(masterDisabledInventory.skills.find(item => item.skill.name === 'user-only').enabled, false);
  assert.equal(masterDisabledInventory.skills.find(item => item.skill.name === 'shared').selected, false);
  const masterDisabled = await registry.snapshot();
  assert.deepEqual(masterDisabled.skills, []);

  registry.setActive(true);
  const restored = await registry.snapshot();
  assert.deepEqual(restored.skills.map(skill => skill.name), ['claude-only', 'user-only']);

  const shadowed = after.diagnostics.find(item => item.code === 'SKILL_SHADOWED' && item.name === 'shared');
  assert.equal(shadowed.source, 'codex-user');
  assert.equal(shadowed.scope, 'user');
  assert.equal(shadowed.path, '~/.agents/skills/shared/SKILL.md');
});

test('Skill registry revision changes when project instructions change', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-skills-revision-'));
  await writeSkill(root, path.join('skills', 'demo'), 'demo', 'First description', 'First body');
  const registry = new SkillRegistry(root, { homeDir: null });
  const first = await registry.snapshot();
  await writeSkill(root, path.join('skills', 'demo'), 'demo', 'Second description', 'Second body');
  const second = await registry.snapshot();
  assert.notEqual(first.revision, second.revision);
  assert.equal(second.skills[0].description, 'Second description');
});
