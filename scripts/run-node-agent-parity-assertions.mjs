import { spawn } from 'node:child_process';
import { access, readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const scriptPath = fileURLToPath(import.meta.url);
const defaultRoot = path.resolve(path.dirname(scriptPath), '..');

function outputTail(value, maxBytes = 4_000) {
  const text = String(value ?? '').trim();
  return Buffer.byteLength(text) <= maxBytes ? text : `...${Buffer.from(text).subarray(-maxBytes).toString()}`;
}

function run(program, args, cwd) {
  return new Promise(resolve => {
    const child = spawn(program, args, {
      cwd,
      windowsHide: true,
      stdio: ['ignore', 'pipe', 'pipe']
    });
    let stdout = '';
    let stderr = '';
    child.stdout.on('data', chunk => { stdout += chunk.toString(); });
    child.stderr.on('data', chunk => { stderr += chunk.toString(); });
    child.once('error', error => resolve({ code: -1, stdout, stderr: `${stderr}\n${error.message}` }));
    child.once('exit', code => resolve({ code: code ?? -1, stdout, stderr }));
  });
}

async function evidenceResult(root, assertion) {
  for (const file of assertion.test_files ?? []) {
    const absolute = path.resolve(root, file);
    try {
      await access(absolute);
      const source = await readFile(absolute, 'utf8');
      if (!/\btest(?:\.\w+)?\s*\(/.test(source)) {
        return { id: assertion.id, item_ids: assertion.item_ids, status: 'failed', message: `${file} has no Node test declaration` };
      }
    } catch (error) {
      return { id: assertion.id, item_ids: assertion.item_ids, status: 'failed', message: `${file}: ${error instanceof Error ? error.message : String(error)}` };
    }
  }
  return { id: assertion.id, item_ids: assertion.item_ids, status: 'passed', message: 'test evidence is present' };
}

export async function runBehavioralAssertions({ root = defaultRoot, registry, build = true } = {}) {
  const definitions = Array.isArray(registry?.assertions) ? registry.assertions : [];
  const results = definitions
    .filter(value => value.mode === 'planned')
    .map(assertion => ({
      id: assertion.id,
      item_ids: assertion.item_ids,
      status: 'planned',
      message: `planned fixture: ${(assertion.test_files ?? []).join(', ')}`
    }));
  for (const assertion of definitions.filter(value => value.mode === 'evidence')) {
    results.push(await evidenceResult(root, assertion));
  }

  const executable = definitions.filter(value => value.mode === 'node_test');
  if (!executable.length) return results;
  const packageRoot = path.resolve(root, 'packages', 'node-agent');
  const preflightFailures = new Map();
  for (const assertion of executable.filter(value => value.preflight === 'rust_contract')) {
    const checker = path.resolve(packageRoot, 'scripts', 'sync-rust-catalog.mjs');
    const outcome = await run(process.execPath, [checker, '--check'], packageRoot);
    if (outcome.code !== 0) {
      preflightFailures.set(
        assertion.id,
        outputTail(outcome.stderr || outcome.stdout) || `Rust contract check exited ${outcome.code}`
      );
    }
  }
  if (build) {
    const compiled = process.platform === 'win32'
      ? await run(process.env.ComSpec ?? 'cmd.exe', ['/d', '/s', '/c', 'npm.cmd run build:server'], packageRoot)
      : await run('npm', ['run', 'build:server'], packageRoot);
    if (compiled.code !== 0) {
      const message = outputTail(compiled.stderr || compiled.stdout) || `build exited ${compiled.code}`;
      return results.concat(executable.map(assertion => ({
        id: assertion.id,
        item_ids: assertion.item_ids,
        status: 'failed',
        message: `Node Agent build failed: ${message}`
      })));
    }
  }

  for (const assertion of executable) {
    const preflightFailure = preflightFailures.get(assertion.id);
    if (preflightFailure) {
      results.push({
        id: assertion.id,
        item_ids: assertion.item_ids,
        status: 'failed',
        message: `Rust contract check failed: ${preflightFailure}`
      });
      continue;
    }
    const files = (assertion.test_files ?? []).map(file => path.resolve(root, file));
    if (!files.length) {
      results.push({ id: assertion.id, item_ids: assertion.item_ids, status: 'skipped', message: 'no test_files declared' });
      continue;
    }
    const outcome = await run(process.execPath, ['--test', ...files], packageRoot);
    const detail = outputTail(outcome.stderr || outcome.stdout);
    results.push({
      id: assertion.id,
      item_ids: assertion.item_ids,
      status: outcome.code === 0 ? 'passed' : 'failed',
      message: outcome.code === 0 ? 'Node test fixture passed' : detail || `Node test exited ${outcome.code}`
    });
  }
  return results;
}

async function main() {
  const registryPath = path.resolve(defaultRoot, 'docs', 'todo', 'node-agent-parity', 'assertions.json');
  const registry = JSON.parse(await readFile(registryPath, 'utf8'));
  const results = await runBehavioralAssertions({ root: defaultRoot, registry });
  console.log(JSON.stringify(results, null, 2));
  process.exitCode = results.some(result => ['failed', 'skipped'].includes(result.status)) ? 1 : 0;
}

if (process.argv[1] && pathToFileURL(path.resolve(process.argv[1])).href === import.meta.url) {
  main().catch(error => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
