import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

test('Rust and Node portable builds keep versioned ZIPs and stable expanded directories', async () => {
  const [rust, node] = await Promise.all([
    readFile(new URL('../scripts/build-portable.ps1', import.meta.url), 'utf8'),
    readFile(new URL('../packages/node-agent/scripts/build-portable.ps1', import.meta.url), 'utf8')
  ]);

  assert.match(rust, /Coding\.Tools\.MCP_\$\{version\}_x64_portable/);
  assert.match(rust, /\$expandedName = 'Coding\.Tools\.MCP_x64_portable'/);
  assert.match(rust, /\$zipPath = Join-Path \$distRoot "\$packageName\.zip"/);
  assert.match(rust, /\$expandedDir = Join-Path \$distRoot \$expandedName/);

  assert.match(node, /\$zipPath = Join-Path \$OutputDirectory "\$packageName\.zip"/);
  assert.match(node, /\$expandedPath = Join-Path \$OutputDirectory \$ExpandedName/);
  assert.match(node, /expandedName = 'Coding\.Tools\.Node\.Agent_portable_bundled-node_win-x64'/);
  assert.match(node, /expandedName = 'Coding\.Tools\.Node\.Agent_portable_system-node_win-x64'/);
});
