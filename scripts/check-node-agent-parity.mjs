import { execFile } from 'node:child_process';
import { access, readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { promisify } from 'node:util';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { runBehavioralAssertions } from './run-node-agent-parity-assertions.mjs';

const execFileAsync = promisify(execFile);
const scriptPath = fileURLToPath(import.meta.url);
const workspace = resolve(dirname(scriptPath), '..');
const roadmapDir = resolve(workspace, 'docs', 'todo', 'node-agent-parity');
const manifestPath = resolve(roadmapDir, 'manifest.json');
const SHA_PATTERN = /^[0-9a-f]{40}$/;
const TEST_DECLARATION_PATTERN = /\btest(?:\.\w+)?\s*\(/;

async function exists(path) {
  try { await access(path); return true; } catch { return false; }
}

async function readJson(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

async function git(root, args) {
  const result = await execFileAsync('git', args, { cwd: root, windowsHide: true });
  return result.stdout.trim();
}

async function resolveRuntimeBaseline(root) {
  const [repositoryHead, desktop, agent] = await Promise.all([
    git(root, ['rev-parse', 'HEAD']),
    readJson(resolve(root, 'package.json')),
    readJson(resolve(root, 'packages', 'node-agent', 'package.json'))
  ]);
  return {
    repositoryHead,
    desktopClientVersion: desktop.version,
    nodeAgentVersion: agent.version,
    clientCompatibilityVersion: agent.codingTools?.clientVersion
  };
}

async function isAncestor(root, anchor, head) {
  try {
    await execFileAsync('git', ['merge-base', '--is-ancestor', anchor, head], { cwd: root, windowsHide: true });
    return true;
  } catch {
    return false;
  }
}

function cycleErrors(items) {
  const byId = new Map(items.map(item => [item.id, item]));
  const visiting = new Set();
  const visited = new Set();
  const errors = [];
  function visit(id, chain = []) {
    if (visited.has(id)) return;
    if (visiting.has(id)) {
      errors.push(`dependency cycle: ${[...chain, id].join(' -> ')}`);
      return;
    }
    visiting.add(id);
    for (const dependency of byId.get(id)?.depends_on ?? []) visit(dependency, [...chain, id]);
    visiting.delete(id);
    visited.add(id);
  }
  for (const item of items) visit(item.id);
  return errors;
}

function assertionDiagnostic(assertion, result) {
  const owners = (assertion.item_ids ?? []).join(', ') || 'unowned';
  return `${owners}/${assertion.id}: ${result?.message ?? `required assertion result is ${result?.status ?? 'missing'}`}`;
}

export async function validateParity(root = workspace, manifestFile = manifestPath, options = {}) {
  const errors = [];
  const warnings = [];
  const manifest = await readJson(manifestFile);
  const effectiveRoadmapDir = options.roadmapDirectory ?? dirname(manifestFile);
  const registryFile = resolve(effectiveRoadmapDir, manifest.assertion_registry ?? 'assertions.json');
  const registry = await readJson(registryFile).catch(error => {
    errors.push(`assertion registry could not be read: ${error instanceof Error ? error.message : String(error)}`);
    return { schema_version: 0, assertions: [], required_categories: [] };
  });
  const allowedStatuses = new Set(manifest.status_values ?? []);
  const allowedPriorities = new Set(manifest.priority_values ?? []);
  const items = Array.isArray(manifest.items) ? manifest.items : [];
  const ids = new Set();
  const files = new Set();

  if (manifest.schema_version !== 2) errors.push('manifest schema_version must be 2');
  if (!items.length) errors.push('manifest items must not be empty');
  if (registry.schema_version !== 1) errors.push('assertion registry schema_version must be 1');

  const assertions = Array.isArray(registry.assertions) ? registry.assertions : [];
  const assertionById = new Map();
  for (const assertion of assertions) {
    if (!/^PA-[A-Z0-9-]+$/.test(assertion.id ?? '')) errors.push(`invalid assertion id: ${String(assertion.id)}`);
    if (assertionById.has(assertion.id)) errors.push(`duplicate assertion id: ${assertion.id}`);
    assertionById.set(assertion.id, assertion);
    if (!['evidence', 'node_test', 'planned'].includes(assertion.mode)) errors.push(`${assertion.id}: invalid mode ${assertion.mode}`);
    if (assertion.preflight !== undefined && assertion.preflight !== 'rust_contract') errors.push(`${assertion.id}: invalid preflight ${assertion.preflight}`);
    if (assertion.category === 'rust_fixture' && assertion.preflight !== 'rust_contract') errors.push(`${assertion.id}: Rust fixture assertions require the rust_contract preflight`);
    if (assertion.mode === 'planned') {
      if (assertion.required !== false) errors.push(`${assertion.id}: planned assertions must set required to false`);
      if (assertion.preflight !== undefined) errors.push(`${assertion.id}: planned assertions cannot declare preflight`);
    } else if (assertion.required !== true) {
      errors.push(`${assertion.id}: executable and evidence assertions must set required to true`);
    }
    if (!Array.isArray(assertion.item_ids) || !assertion.item_ids.length) errors.push(`${assertion.id}: item_ids must not be empty`);
    if (!Array.isArray(assertion.test_files) || !assertion.test_files.length) errors.push(`${assertion.id}: test_files must not be empty`);
    for (const testFile of assertion.mode === 'planned' ? [] : (assertion.test_files ?? [])) {
      const absolute = resolve(root, testFile);
      if (!(await exists(absolute))) {
        errors.push(`${assertion.id}: missing test evidence ${testFile}`);
      } else if (!TEST_DECLARATION_PATTERN.test(await readFile(absolute, 'utf8'))) {
        errors.push(`${assertion.id}: test evidence has no Node test declaration: ${testFile}`);
      }
    }
  }

  for (const category of registry.required_categories ?? []) {
    if (!assertions.some(assertion => assertion.category === category && assertion.mode === 'node_test' && assertion.required === true)) {
      errors.push(`required behavioral category has no executable assertion: ${category}`);
    }
  }

  for (const item of items) {
    if (!/^NP-\d{3}$/.test(item.id ?? '')) errors.push(`invalid item id: ${String(item.id)}`);
    if (ids.has(item.id)) errors.push(`duplicate item id: ${item.id}`);
    ids.add(item.id);
    if (files.has(item.file)) errors.push(`duplicate item file: ${item.file}`);
    files.add(item.file);
    if (!allowedStatuses.has(item.status)) errors.push(`${item.id}: invalid status ${item.status}`);
    if (!allowedPriorities.has(item.priority)) errors.push(`${item.id}: invalid priority ${item.priority}`);
    if (!Array.isArray(item.depends_on)) errors.push(`${item.id}: depends_on must be an array`);
    if (!Array.isArray(item.rust_sources) || !item.rust_sources.length) errors.push(`${item.id}: rust_sources must not be empty`);
    if (!Array.isArray(item.node_sources) || !item.node_sources.length) errors.push(`${item.id}: node_sources must not be empty`);
    if (!Array.isArray(item.acceptance_tests) || !item.acceptance_tests.length) errors.push(`${item.id}: acceptance_tests must not be empty`);
    if (item.status === 'blocked' && !String(item.blocked_reason ?? '').trim()) errors.push(`${item.id}: blocked_reason is required`);

    if (item.area !== 'product-boundary') {
      if (!Array.isArray(item.assertion_ids) || !item.assertion_ids.length) {
        errors.push(`${item.id}: non-product item must declare assertion_ids`);
      }
      for (const assertionId of item.assertion_ids ?? []) {
        const assertion = assertionById.get(assertionId);
        if (!assertion) errors.push(`${item.id}/${assertionId}: unknown assertion`);
        else if (!(assertion.item_ids ?? []).includes(item.id)) errors.push(`${item.id}/${assertionId}: assertion does not declare the item owner`);
        else if (['done', 'excluded'].includes(item.status) && assertion.mode === 'planned') errors.push(`${item.id}/${assertionId}: completed item cannot use a planned assertion`);
        else if (['done', 'excluded'].includes(item.status) && assertion.required !== true) errors.push(`${item.id}/${assertionId}: completed item assertion is not required`);
      }
    }

    const todoPath = resolve(effectiveRoadmapDir, item.file ?? '');
    if (!(await exists(todoPath))) {
      errors.push(`${item.id}: missing TODO file ${item.file}`);
    } else {
      const markdown = await readFile(todoPath, 'utf8');
      const idMarker = markdown.match(/<!--\s*parity-id:\s*([^\s]+)\s*-->/)?.[1];
      const statusMarker = markdown.match(/<!--\s*parity-status:\s*([^\s]+)\s*-->/)?.[1];
      if (idMarker !== item.id) errors.push(`${item.id}: parity-id marker mismatch`);
      if (statusMarker !== item.status) errors.push(`${item.id}: parity-status marker ${statusMarker ?? 'missing'} does not match ${item.status}`);
      for (const heading of ['## Gap', '## Rust evidence', '## Node current state']) {
        if (!markdown.includes(heading) && !(item.status === 'blocked' && heading === '## Gap')) errors.push(`${item.id}: missing heading ${heading}`);
      }
      if (!markdown.includes('- [ ]') && item.status !== 'done' && item.status !== 'excluded') warnings.push(`${item.id}: no open acceptance checklist found`);
      if (item.status === 'done' && markdown.includes('- [ ]')) errors.push(`${item.id}: done item still has unchecked tasks`);
    }

    for (const source of [...(item.rust_sources ?? []), ...(item.node_sources ?? [])]) {
      if (!(await exists(resolve(root, source)))) errors.push(`${item.id}: missing source reference ${source}`);
    }
  }

  for (const assertion of assertions) {
    for (const itemId of assertion.item_ids ?? []) {
      const owner = items.find(item => item.id === itemId);
      if (!owner) errors.push(`${assertion.id}: unknown item owner ${itemId}`);
      else if (owner.area !== 'product-boundary' && !(owner.assertion_ids ?? []).includes(assertion.id)) {
        errors.push(`${itemId}/${assertion.id}: item does not link back to assertion`);
      } else if (assertion.mode === 'planned' && ['done', 'excluded'].includes(owner.status)) {
        errors.push(`${itemId}/${assertion.id}: planned assertion owner must remain incomplete`);
      }
    }
  }

  for (const divergence of manifest.intentional_divergences ?? []) {
    if (!Array.isArray(divergence.shared_assertion_ids)) {
      errors.push(`${divergence.id}: shared_assertion_ids must be explicit`);
      continue;
    }
    for (const assertionId of divergence.shared_assertion_ids) {
      if (!assertionById.has(assertionId)) errors.push(`${divergence.id}/${assertionId}: unknown shared assertion`);
    }
  }

  for (const item of items) {
    for (const dependency of item.depends_on ?? []) {
      if (!ids.has(dependency)) errors.push(`${item.id}: unknown dependency ${dependency}`);
      if (dependency === item.id) errors.push(`${item.id}: cannot depend on itself`);
    }
  }
  errors.push(...cycleErrors(items));
  const byId = new Map(items.map(item => [item.id, item]));
  for (const item of items.filter(value => ['done', 'excluded'].includes(value.status))) {
    for (const dependency of item.depends_on ?? []) {
      if (!['done', 'excluded'].includes(byId.get(dependency)?.status)) {
        errors.push(`${item.id}: completed item depends on incomplete ${dependency}`);
      }
    }
  }

  const runtime = options.runtimeBaseline ?? await resolveRuntimeBaseline(root);
  const baseline = manifest.baseline ?? {};
  const baselineStatus = {
    anchor: baseline.repository_head,
    current: runtime.repositoryHead,
    headPolicy: baseline.head_policy,
    ancestor: false,
    desktopClientVersion: runtime.desktopClientVersion,
    nodeAgentVersion: runtime.nodeAgentVersion,
    clientCompatibilityVersion: runtime.clientCompatibilityVersion
  };
  if (!SHA_PATTERN.test(baseline.repository_head ?? '')) errors.push('baseline repository_head must be a 40-character lowercase commit SHA');
  if (baseline.head_policy !== 'ancestor') errors.push('baseline head_policy must be ancestor');
  if (SHA_PATTERN.test(baseline.repository_head ?? '') && SHA_PATTERN.test(runtime.repositoryHead ?? '')) {
    baselineStatus.ancestor = typeof options.isAncestor === 'boolean'
      ? options.isAncestor
      : await isAncestor(root, baseline.repository_head, runtime.repositoryHead);
    if (!baselineStatus.ancestor) errors.push(`baseline repository_head ${baseline.repository_head} is not an ancestor of HEAD ${runtime.repositoryHead}`);
  }
  if (baseline.desktop_client_version !== runtime.desktopClientVersion) {
    errors.push(`baseline desktop_client_version ${baseline.desktop_client_version ?? 'missing'} does not match ${runtime.desktopClientVersion}`);
  }
  if (baseline.node_agent_version !== runtime.nodeAgentVersion) {
    errors.push(`baseline node_agent_version ${baseline.node_agent_version ?? 'missing'} does not match ${runtime.nodeAgentVersion}`);
  }
  if (baseline.client_compatibility_version !== runtime.clientCompatibilityVersion) {
    errors.push(`baseline client_compatibility_version ${baseline.client_compatibility_version ?? 'missing'} does not match ${runtime.clientCompatibilityVersion}`);
  }

  let assertionResults = assertions
    .filter(assertion => ['evidence', 'planned'].includes(assertion.mode))
    .map(assertion => ({
      id: assertion.id,
      item_ids: assertion.item_ids,
      status: assertion.mode === 'planned' ? 'planned' : 'passed',
      message: assertion.mode === 'planned'
        ? `planned fixture: ${(assertion.test_files ?? []).join(', ')}`
        : 'test evidence is present'
    }));
  if (options.runAssertions) {
    const executed = options.assertionResults ?? await runBehavioralAssertions({
      root,
      registry,
      build: options.assertionBuild !== false
    });
    const byAssertion = new Map(assertionResults.map(result => [result.id, result]));
    for (const result of executed) byAssertion.set(result.id, result);
    assertionResults = [...byAssertion.values()];
    for (const assertion of assertions.filter(value => value.required === true)) {
      const result = byAssertion.get(assertion.id);
      if (!result) errors.push(assertionDiagnostic(assertion));
      else if (result.status !== 'passed') errors.push(assertionDiagnostic(assertion, result));
    }
  }

  const ready = items.filter(item => item.status === 'todo' && item.depends_on.every(id => ['done', 'excluded'].includes(byId.get(id)?.status)));
  const pending = items.filter(item => !['done', 'excluded'].includes(item.status));
  const counts = Object.fromEntries([...allowedStatuses].map(status => [status, items.filter(item => item.status === status).length]));
  return { manifest, registry, items, assertions, assertionResults, baselineStatus, errors, warnings, ready, pending, counts };
}

async function main() {
  const requireComplete = process.argv.includes('--require-complete');
  const json = process.argv.includes('--json');
  const structureOnly = process.argv.includes('--structure-only');
  const skipBuild = process.argv.includes('--skip-build');
  const result = await validateParity(workspace, manifestPath, {
    runAssertions: !structureOnly,
    assertionBuild: !skipBuild
  });
  const completionError = requireComplete && result.pending.length > 0;
  const failed = result.errors.length > 0 || completionError;
  const assertionCounts = result.assertionResults.reduce((counts, value) => {
    counts[value.status] = (counts[value.status] ?? 0) + 1;
    return counts;
  }, {});
  if (json) {
    console.log(JSON.stringify({
      ok: !failed,
      counts: result.counts,
      pending: result.pending.map(item => item.id),
      ready: result.ready.map(item => item.id),
      baseline: result.baselineStatus,
      assertions: result.assertionResults,
      errors: result.errors,
      warnings: result.warnings
    }, null, 2));
  } else {
    console.log(`Node Agent parity roadmap: ${result.items.length} items`);
    console.log(`Status: ${Object.entries(result.counts).map(([status, count]) => `${status}=${count}`).join(', ')}`);
    console.log(`Ready: ${result.ready.map(item => item.id).join(', ') || 'none'}`);
    console.log(`Baseline: ${result.baselineStatus.anchor} <= ${result.baselineStatus.current}; Desktop ${result.baselineStatus.desktopClientVersion}; Agent ${result.baselineStatus.nodeAgentVersion}`);
    console.log(`Behavioral assertions: ${Object.entries(assertionCounts).map(([status, count]) => `${status}=${count}`).join(', ') || 'not run'}`);
    if (result.pending[0]) console.log(`Next planned item: ${result.ready[0]?.id ?? result.pending[0].id} — ${(result.ready[0] ?? result.pending[0]).title}`);
    for (const warning of result.warnings) console.warn(`warning: ${warning}`);
    for (const error of result.errors) console.error(`error: ${error}`);
    if (completionError) console.error(`error: ${result.pending.length} parity items are not complete: ${result.pending.map(item => item.id).join(', ')}`);
  }
  process.exitCode = failed ? 1 : 0;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  main().catch(error => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
