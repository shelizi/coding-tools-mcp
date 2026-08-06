import test from 'node:test';
import assert from 'node:assert/strict';
import { access, mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { EDIT_PROPOSAL_TTL_MS } from '../dist/editRecovery.js';
import { createToolContext } from '../dist/server.js';
import { callTool } from '../dist/tools.js';

function config(root, dataDir) {
  return {
    host: '127.0.0.1',
    port: 0,
    dataDir,
    permissionMode: 'trusted',
    management: { enabled: false },
    oauth: {
      clientId: 'chatgpt',
      password: 'edit-recovery-password',
      tokenSecret: 'edit-recovery-token-secret'
    },
    folders: [{ id: 'repo', name: 'Repo', path: root }],
    limits: {
      blockingConcurrency: 4,
      processConcurrency: 4,
      activeSessionLimit: 16,
      maxOutputBytes: 1024 * 1024
    }
  };
}

async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'ctmcp-edit-recovery-root-'));
  const dataDir = await mkdtemp(path.join(tmpdir(), 'ctmcp-edit-recovery-data-'));
  t.after(async () => {
    await rm(root, { recursive: true, force: true });
    await rm(dataDir, { recursive: true, force: true });
  });
  const ctx = await createToolContext(config(root, dataDir));
  const meta = { 'openai/session': `edit-recovery-${Date.now()}-${Math.random()}` };
  const selected = await callTool(ctx, 'switch_workspace_folder', { folder_id: 'repo' }, meta);
  assert.equal(selected.ok, true);
  return { root, ctx, meta };
}

async function proposal(ctx, meta, file = 'main.txt', replacement = 'let value = 2;') {
  const result = await callTool(ctx, 'edit_file', {
    path: file,
    edits: [{
      type: 'replace',
      old_text: 'let value = 1;',
      new_text: replacement
    }]
  }, meta);
  assert.equal(result.ok, true, JSON.stringify(result));
  assert.equal(result.status, 'proposal_required');
  return result;
}

test('ambiguous exact edit returns a bounded proposal without writing', async t => {
  const { root, ctx, meta } = await fixture(t);
  const file = path.join(root, 'main.txt');
  await writeFile(file, 'let  value = 1;\n');

  const result = await proposal(ctx, meta);
  assert.equal(result.applied, false);
  assert.equal(result.proposal_ttl_seconds, 300);
  assert.match(result.proposal_id, /^[0-9a-f]{32}$/);
  assert.equal(result.actual_text, 'let  value = 1;');
  assert.equal(result.requested_old_text, 'let value = 1;');
  assert.equal(result.requested_new_text, 'let value = 2;');
  assert.equal(result.proposed_content, 'let value = 2;\n');
  assert.equal(result.proposed_content_included, true);
  assert.match(result.proposed_content_sha256, /^[0-9a-f]{64}$/);
  assert.deepEqual(result.accepted_formats, ['accept', 'replacement', 'patch']);
  assert.equal(result.preferred_format, 'replacement');
  assert.equal(result.proposal_patch_format, 'unified_diff_single_file_single_hunk');
  assert.equal(result.next_action, 'apply_proposal');
  assert.equal(await readFile(file, 'utf8'), 'let  value = 1;\n');
  assert.equal(ctx.editProposals.size, 1);
});

test('precise edit contracts aggregate invalid fields with guarded recovery metadata', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'main.txt'), 'old\n');

  const invalid = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    edits: [{ type: 'replace', old_text: 'old', anchor: 'unexpected' }]
  }, meta);
  assert.equal(invalid.ok, false, JSON.stringify(invalid));
  assert.equal(invalid.error.code, 'EDIT_CONTRACT_INVALID');
  assert.equal(invalid.error.details.issue_count, 2);
  assert.equal(invalid.error.details.path, 'main.txt');
  assert.match(invalid.error.details.actual_sha256, /^[0-9a-f]{64}$/);
  assert.equal(invalid.error.details.recovery_actions[1].tool, 'edit');
  assert.equal(await readFile(path.join(root, 'main.txt'), 'utf8'), 'old\n');

  const mixed = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    edits: [{ type: 'replace', old_text: 'old', new_text: 'new' }],
    apply_proposal: { proposal_id: '0'.repeat(32) }
  }, meta);
  assert.equal(mixed.ok, false, JSON.stringify(mixed));
  assert.equal(mixed.error.code, 'EDIT_CONTRACT_INVALID');
  assert.equal(mixed.error.details.recovery_actions[0].action, 'choose_edit_mode');
});

test('edit_many reports the failed file index before writing any file', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'first.txt'), 'first\n');
  await writeFile(path.join(root, 'second.txt'), 'second\n');

  const result = await callTool(ctx, 'edit_many', {
    files: [
      {
        path: 'first.txt',
        edits: [{ type: 'replace', old_text: 'first', new_text: 'FIRST' }]
      },
      {
        path: 'second.txt',
        edits: [{ type: 'replace', old_text: 'second', anchor: 'unexpected' }]
      }
    ]
  }, meta);
  assert.equal(result.ok, false, JSON.stringify(result));
  assert.equal(result.error.code, 'EDIT_CONTRACT_INVALID');
  assert.equal(result.error.details.file_index, 1);
  assert.equal(result.error.details.path, 'second.txt');
  assert.equal(await readFile(path.join(root, 'first.txt'), 'utf8'), 'first\n');
  assert.equal(await readFile(path.join(root, 'second.txt'), 'utf8'), 'second\n');
});

test('delete_lines applies the same expected_text guard as replace_lines', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'main.txt'), 'alpha\nbeta\ngamma\n');

  const result = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    edits: [{
      type: 'delete_lines',
      start_line: 2,
      end_line: 2,
      expected_text: 'different'
    }]
  }, meta);
  assert.equal(result.ok, false, JSON.stringify(result));
  assert.equal(result.error.code, 'EDIT_EXPECTED_TEXT_MISMATCH');
  assert.equal(result.error.details.actual_text, 'beta');
  assert.equal(await readFile(path.join(root, 'main.txt'), 'utf8'), 'alpha\nbeta\ngamma\n');
});

test('dry-run edit plans replay once and reject stale reuse', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'main.txt'), 'old\n');

  const planned = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    dry_run: true,
    reason: 'guarded replay test',
    edits: [{ type: 'replace', old_text: 'old', new_text: 'new' }]
  }, meta);
  assert.equal(planned.ok, true, JSON.stringify(planned));
  assert.equal(planned.applied, false);
  assert.equal(planned.edit_plan.tool, 'edit');
  assert.equal(planned.edit_plan.arguments.dry_run, false);
  assert.equal(planned.edit_plan.arguments.reason, 'guarded replay test');
  assert.equal(planned.edit_plan.arguments.files[0].expected_sha256, planned.before_sha256);
  assert.equal(planned.edit_plan.expected_result.files[0].after_sha256, planned.after_sha256);
  assert.match(planned.edit_plan.plan_sha256, /^[0-9a-f]{64}$/);
  assert.deepEqual(planned.edit_plan.stateful_dependencies, []);

  const replayed = await callTool(ctx, planned.edit_plan.tool, planned.edit_plan.arguments, meta);
  assert.equal(replayed.ok, true, JSON.stringify(replayed));
  assert.equal(replayed.applied, true);
  assert.equal(await readFile(path.join(root, 'main.txt'), 'utf8'), 'new\n');

  const stale = await callTool(ctx, planned.edit_plan.tool, planned.edit_plan.arguments, meta);
  assert.equal(stale.ok, false, JSON.stringify(stale));
  assert.equal(stale.error.code, 'FILE_VERSION_MISMATCH');
});

test('dry-run edit_many plans replay atomically with per-file guards', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'first.txt'), 'first\n');
  await writeFile(path.join(root, 'second.txt'), 'second\n');

  const planned = await callTool(ctx, 'edit_many', {
    dry_run: true,
    files: [
      { path: 'first.txt', edits: [{ type: 'replace', old_text: 'first', new_text: 'FIRST' }] },
      { path: 'second.txt', edits: [{ type: 'replace', old_text: 'second', new_text: 'SECOND' }] }
    ]
  }, meta);
  assert.equal(planned.ok, true, JSON.stringify(planned));
  assert.equal(planned.edit_plan.tool, 'edit');
  assert.equal(planned.edit_plan.arguments.dry_run, false);
  assert.equal(planned.edit_plan.arguments.files.length, 2);
  assert.match(planned.edit_plan.plan_sha256, /^[0-9a-f]{64}$/);

  const replayed = await callTool(ctx, planned.edit_plan.tool, planned.edit_plan.arguments, meta);
  assert.equal(replayed.ok, true, JSON.stringify(replayed));
  assert.equal(await readFile(path.join(root, 'first.txt'), 'utf8'), 'FIRST\n');
  assert.equal(await readFile(path.join(root, 'second.txt'), 'utf8'), 'SECOND\n');
 });

test('edit returns candidate context, supports context disambiguation, and edits explicit multiple matches', async t => {
  const { root, ctx, meta } = await fixture(t);
  const file = path.join(root, 'main.txt');
  const original = 'fn first() {\n  return value;\n}\n\nfn second() {\n  return value;\n}\n';
  await writeFile(file, original);

  const ambiguous = await callTool(ctx, 'edit', {
    files: [{
      path: 'main.txt',
      edits: [{ type: 'replace', old_text: 'return value;', new_text: 'return result;' }]
    }]
  }, meta);
  assert.equal(ambiguous.ok, false, JSON.stringify(ambiguous));
  assert.equal(ambiguous.error.code, 'EDIT_MATCH_COUNT_MISMATCH');
  assert.equal(ambiguous.error.details.actual_occurrences, 2);
  assert.deepEqual(ambiguous.error.details.candidate_lines, [2, 6]);
  assert.equal(ambiguous.error.details.candidate_contexts.length, 2);
  assert.equal(ambiguous.error.details.candidate_contexts_truncated, false);
  assert.equal(await readFile(file, 'utf8'), original);

  const selected = await callTool(ctx, 'edit', {
    files: [{
      path: 'main.txt',
      edits: [{
        type: 'replace',
        old_text: 'return value;',
        before_context: 'fn second() {\n  ',
        after_context: '\n}',
        new_text: 'return result;'
      }]
    }]
  }, meta);
  assert.equal(selected.ok, true, JSON.stringify(selected));
  assert.equal(selected.atomic, true);
  assert.equal(
    await readFile(file, 'utf8'),
    'fn first() {\n  return value;\n}\n\nfn second() {\n  return result;\n}\n'
  );

  await writeFile(file, original);
  const all = await callTool(ctx, 'edit', {
    files: [{
      path: 'main.txt',
      edits: [{
        type: 'replace',
        old_text: 'return value;',
        new_text: 'return result;',
        expected_occurrences: 2
      }]
    }]
  }, meta);
  assert.equal(all.ok, true, JSON.stringify(all));
  assert.equal(all.atomic, true);
  assert.equal(
    await readFile(file, 'utf8'),
    'fn first() {\n  return result;\n}\n\nfn second() {\n  return result;\n}\n'
  );
});

test('proposal accept and replacement modes apply atomically and consume the proposal', async t => {
  const { root, ctx, meta } = await fixture(t);
  const file = path.join(root, 'main.txt');
  await writeFile(file, 'let  value = 1;\n');

  const acceptedProposal = await proposal(ctx, meta);
  const accepted = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: { proposal_id: acceptedProposal.proposal_id }
  }, meta);
  assert.equal(accepted.ok, true, JSON.stringify(accepted));
  assert.equal(accepted.status, 'proposal_applied');
  assert.equal(accepted.proposal_apply_format, 'accept');
  assert.equal(accepted.applied, true);
  assert.match(accepted.diff, /\+let value = 2;/);
  assert.equal(await readFile(file, 'utf8'), 'let value = 2;\n');
  assert.equal(ctx.editProposals.has(acceptedProposal.proposal_id), false);

  await writeFile(file, 'let  value = 1;\n');
  const dryRunProposal = await proposal(ctx, meta);
  const dryRun = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    dry_run: true,
    apply_proposal: { proposal_id: dryRunProposal.proposal_id }
  }, meta);
  assert.equal(dryRun.ok, true, JSON.stringify(dryRun));
  assert.equal(dryRun.dry_run, true);
  assert.equal(dryRun.applied, false);
  assert.equal(ctx.editProposals.has(dryRunProposal.proposal_id), true);
  assert.equal(await readFile(file, 'utf8'), 'let  value = 1;\n');

  await writeFile(file, 'let  value = 1;\n');
  const replacementProposal = await proposal(ctx, meta);
  const replaced = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: {
      proposal_id: replacementProposal.proposal_id,
      replacement: 'let value = 3;'
    }
  }, meta);
  assert.equal(replaced.ok, true, JSON.stringify(replaced));
  assert.equal(replaced.proposal_apply_format, 'replacement');
  assert.equal(await readFile(file, 'utf8'), 'let value = 3;\n');
});

test('proposal rejects changed files, expired IDs and missing IDs', async t => {
  const { root, ctx, meta } = await fixture(t);
  const file = path.join(root, 'main.txt');
  await writeFile(file, 'let  value = 1;\n');

  const staleProposal = await proposal(ctx, meta);
  await writeFile(file, 'let  value = 9;\n');
  const stale = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: { proposal_id: staleProposal.proposal_id }
  }, meta);
  assert.equal(stale.ok, false);
  assert.equal(stale.error.code, 'EDIT_PROPOSAL_STALE');
  assert.equal(stale.error.category, 'conflict');
  assert.equal(stale.error.retryable, true);
  assert.equal(stale.error.details.reason, 'file_changed');
  assert.equal(await readFile(file, 'utf8'), 'let  value = 9;\n');

  await writeFile(file, 'let  value = 1;\n');
  const candidateProposal = await proposal(ctx, meta);
  ctx.editProposals.get(candidateProposal.proposal_id).actualText = 'different-candidate';
  const candidateChanged = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: { proposal_id: candidateProposal.proposal_id }
  }, meta);
  assert.equal(candidateChanged.ok, false);
  assert.equal(candidateChanged.error.code, 'EDIT_PROPOSAL_STALE');
  assert.equal(candidateChanged.error.details.reason, 'candidate_changed');

  await writeFile(file, 'let  value = 1;\n');
  const expiredProposal = await proposal(ctx, meta);
  ctx.editProposals.get(expiredProposal.proposal_id).createdAt -= EDIT_PROPOSAL_TTL_MS + 1;
  const expired = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: { proposal_id: expiredProposal.proposal_id }
  }, meta);
  assert.equal(expired.ok, false);
  assert.equal(expired.error.code, 'EDIT_PROPOSAL_NOT_FOUND');
  assert.equal(expired.error.details.reason, 'missing_or_expired');

  const missing = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: { proposal_id: '0'.repeat(32) }
  }, meta);
  assert.equal(missing.ok, false);
  assert.equal(missing.error.code, 'EDIT_PROPOSAL_NOT_FOUND');
});

test('proposal store remains bounded and evicts the oldest proposal', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'main.txt'), 'let  value = 1;\n');
  const ids = [];
  for (let index = 0; index < 201; index += 1) {
    const result = await proposal(ctx, meta, 'main.txt', `let value = ${index + 2};`);
    ids.push(result.proposal_id);
  }
  assert.equal(ctx.editProposals.size, 200);
  assert.equal(ctx.editProposals.has(ids[0]), false);
  assert.equal(ctx.editProposals.has(ids.at(-1)), true);
});

test('large proposals accept an efficient restricted single-hunk patch', async t => {
  const { root, ctx, meta } = await fixture(t);
  const file = path.join(root, 'main.txt');
  await writeFile(file, 'let  value = 1;\n');
  const lines = Array.from({ length: 1200 }, (_, index) => `line-${String(index + 1).padStart(4, '0')}-payload`);
  const replacement = lines.join('\n');
  const result = await proposal(ctx, meta, 'main.txt', replacement);
  assert.equal(result.preferred_format, 'patch');
  assert.equal(result.proposed_content_included, true);

  const patch = [
    '--- a/proposal',
    '+++ b/proposal',
    '@@ -600,1 +600,1 @@',
    `-${lines[599]}`,
    '+LINE-0600-PATCHED',
    ''
  ].join('\n');
  const applied = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: { proposal_id: result.proposal_id, patch }
  }, meta);
  assert.equal(applied.ok, true, JSON.stringify(applied));
  assert.equal(applied.proposal_apply_format, 'patch');
  const content = await readFile(file, 'utf8');
  assert.match(content, /LINE-0600-PATCHED/);
  assert.doesNotMatch(content, new RegExp(lines[599]));
});

test('restricted proposal patches reject multiple hunks and inefficient patches', async t => {
  const { root, ctx, meta } = await fixture(t);
  const file = path.join(root, 'main.txt');
  await writeFile(file, 'let  value = 1;\n');
  const multipleProposal = await proposal(ctx, meta);
  const multiple = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: {
      proposal_id: multipleProposal.proposal_id,
      patch: [
        '--- a/proposal',
        '+++ b/proposal',
        '@@',
        '-let value = 2;',
        '+let value = 3;',
        '@@',
        '-let value = 3;',
        '+let value = 4;',
        ''
      ].join('\n')
    }
  }, meta);
  assert.equal(multiple.ok, false);
  assert.equal(multiple.error.code, 'EDIT_PROPOSAL_PATCH_INVALID');
  assert.equal(multiple.error.details.reason, 'single_file_single_hunk_required');

  const inefficientProposal = await proposal(ctx, meta);
  const inefficient = await callTool(ctx, 'edit_file', {
    path: 'main.txt',
    apply_proposal: {
      proposal_id: inefficientProposal.proposal_id,
      patch: [
        '--- a/proposal',
        '+++ b/proposal',
        '@@',
        '-let value = 2;',
        '+let value = 3;',
        ''
      ].join('\n')
    }
  }, meta);
  assert.equal(inefficient.ok, false);
  assert.equal(inefficient.error.code, 'EDIT_PROPOSAL_PATCH_INEFFICIENT');
  assert.equal(inefficient.error.details.recommended_format, 'replacement');
  assert.equal(inefficient.error.details.recommended_replacement, 'let value = 3;');
  assert.equal(await readFile(file, 'utf8'), 'let  value = 1;\n');
});

test('patch preflight reports ambiguous and multiple hunk failures with recovery actions', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'ambiguous.txt'), 'same\nother\nsame\n');
  const ambiguous = await callTool(ctx, 'patch_check', {
    patch: [
      '--- a/ambiguous.txt',
      '+++ b/ambiguous.txt',
      '@@',
      ' same',
      '+inserted',
      ''
    ].join('\n')
  }, meta);
  assert.equal(ambiguous.ok, false);
  assert.equal(ambiguous.error.code, 'PATCH_CONTEXT_AMBIGUOUS');
  assert.deepEqual(ambiguous.error.details.candidate_lines, [1, 3]);
  assert.equal(ambiguous.error.details.recovery_actions[0].tool, 'edit');

  await writeFile(path.join(root, 'multiple.txt'), 'actual\ncontent\n');
  const multiple = await callTool(ctx, 'patch_check', {
    patch: [
      '--- a/multiple.txt',
      '+++ b/multiple.txt',
      '@@',
      ' missing-one',
      '@@',
      ' missing-two',
      ''
    ].join('\n')
  }, meta);
  assert.equal(multiple.ok, false);
  assert.equal(multiple.error.code, 'PATCH_PREFLIGHT_FAILED');
  assert.equal(multiple.error.details.issue_count, 2);
  assert.equal(multiple.error.details.issues.length, 2);
  assert.equal(multiple.error.details.recovery_actions[0].action, 'switch_to_precise_edits');
});

test('patch expected hashes fail before Git with guarded recovery metadata', async t => {
  const { root, ctx, meta } = await fixture(t);
  await writeFile(path.join(root, 'hash.txt'), 'old\n');
  const result = await callTool(ctx, 'patch_check', {
    patch: [
      '--- a/hash.txt',
      '+++ b/hash.txt',
      '@@ -1,1 +1,1 @@',
      '-old',
      '+new',
      ''
    ].join('\n'),
    expected_sha256: { 'hash.txt': '0'.repeat(64) }
  }, meta);
  assert.equal(result.ok, false);
  assert.equal(result.error.code, 'FILE_VERSION_MISMATCH');
  assert.match(result.error.details.actual_sha256, /^[0-9a-f]{64}$/);
  assert.equal(result.error.details.recovery_actions[0].tool, 'read_file');
  assert.equal(result.error.details.recovery_actions[1].tool, 'edit');
  assert.equal(await readFile(path.join(root, 'hash.txt'), 'utf8'), 'old\n');

  const absent = path.join(root, 'should-not-exist.txt');
  await assert.rejects(access(absent));
});
