import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { toolNames } from '../dist/catalog.js';
import { dispatchDomainTool, registeredDomainToolNames } from '../dist/toolDispatch.js';
import { toolRuntimeFor } from '../dist/toolRuntime.js';

const delegatedDomains = new Set([
  'harness',
  'history',
  'task',
  'filesystem',
  'search',
  'quality',
  'process',
  'git',
  'runtime',
  'desktop'
]);

test('domain handlers depend on the dispatch contract instead of the registry', async () => {
  const dispatcherPaths = ['git', 'history', 'process', 'runtime', 'task', 'workspace'];
  const [registrySource, permissionSource, contractSource, ...dispatcherSources] = await Promise.all([
    readFile(new URL('../src/toolDispatch.ts', import.meta.url), 'utf8'),
    readFile(new URL('../src/permissionTools.ts', import.meta.url), 'utf8'),
    readFile(new URL('../src/toolDispatch/contract.ts', import.meta.url), 'utf8'),
    ...dispatcherPaths.map(name => readFile(new URL(`../src/toolDispatchers/${name}.ts`, import.meta.url), 'utf8'))
  ]);
  assert.match(registrySource, /from ['"]\.\/toolDispatch\/contract\.js['"]/);
  assert.match(registrySource, /export type \{[^}]*ToolDispatchRequest[^}]*ToolHandlerMap[^}]*\} from ['"]\.\/toolDispatch\/contract\.js['"]/s);
  assert.match(permissionSource, /from ['"]\.\/toolDispatch\/contract\.js['"]/);
  assert.doesNotMatch(permissionSource, /from ['"]\.\/toolDispatch\.js['"]/);
  for (const source of dispatcherSources) {
    assert.match(source, /from ['"]\.\.\/toolDispatch\/contract\.js['"]/);
    assert.doesNotMatch(source, /from ['"]\.\.\/toolDispatch\.js['"]/);
  }
  const imports = contractSource.split(/\r?\n/).filter(line => line.startsWith('import '));
  assert.ok(imports.length > 0);
  assert.ok(imports.every(line => line.startsWith('import type ')));
});

test('domain dispatcher covers every delegated runtime domain', () => {
  const expected = toolNames.filter(name => delegatedDomains.has(toolRuntimeFor(name).domain));
  assert.deepEqual(
    [...registeredDomainToolNames()].sort(),
    [...expected].sort()
  );
  assert.equal(registeredDomainToolNames().length, toolNames.length);
});

test('only unknown tools remain outside the module registry', () => {
  const request = { ctx: {}, key: 'test', args: {}, historyArgs: {} };
  assert.equal(dispatchDomainTool('missing_tool', request), undefined);
});
