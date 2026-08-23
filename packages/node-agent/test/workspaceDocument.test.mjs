import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import {
  CANONICAL_SCHEMA_VERSION,
  canonicalToAgentConfigDocument,
  migrateNodeV1Document,
  overlayAgentDocumentOnCanonical,
  parseCanonicalWorkspace,
  serializeCanonicalWorkspace
} from '../dist/workspaceDocument.js';

const fixtureDir = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  '../../../docs/specs/shared-workspace-config/fixtures'
);

async function loadFixture(name) {
  return JSON.parse(await readFile(path.join(fixtureDir, name), 'utf8'));
}

test('canonical fixtures parse and roundtrip without secrets', async () => {
  const minimal = parseCanonicalWorkspace(await loadFixture('minimal.json'));
  assert.equal(minimal.id, 'ws-minimal');
  assert.equal(minimal.bind.port, 3789);

  const full = parseCanonicalWorkspace(await loadFixture('full.json'));
  const roundtrip = parseCanonicalWorkspace(serializeCanonicalWorkspace(full));
  assert.equal(roundtrip.id, full.id);
  assert.deepEqual(roundtrip.folders, full.folders);
  assert.deepEqual(roundtrip.policy.allowedCommands, ['pytest', 'cargo']);

  const desktop = parseCanonicalWorkspace(await loadFixture('desktop-extras.json'));
  assert.equal(desktop.host.desktop.tunnel.type, 'frp');
  const node = parseCanonicalWorkspace(await loadFixture('node-extras.json'));
  assert.equal(node.host.node.management.enabled, true);
});

test('v1 Node documents migrate into canonical v2 and strip secrets', async () => {
  const migrated = migrateNodeV1Document(await loadFixture('from-node-v1.json'), {
    id: 'ws-v1',
    name: 'V1'
  });
  assert.equal(migrated.schemaVersion, CANONICAL_SCHEMA_VERSION);
  assert.equal(migrated.auth.oauthClientId, 'chatgpt-v1');
  assert.equal(migrated.host.node.dataDir, 'C:\\data\\node-v1');
  const serialized = JSON.stringify(serializeCanonicalWorkspace(migrated));
  assert.equal(serialized.includes('must-not-survive-migrate'), false);
  assert.equal(serialized.includes('enroll/SECRET'), false);
});

test('overlaying a Node document onto canonical keeps host.desktop extras', async () => {
  const desktop = parseCanonicalWorkspace(await loadFixture('desktop-extras.json'));
  const document = canonicalToAgentConfigDocument(desktop);
  document.port = 3790;
  const overlaid = overlayAgentDocumentOnCanonical(document, { id: desktop.id, name: desktop.name }, desktop);
  assert.equal(overlaid.bind.port, 3790);
  assert.equal(overlaid.host.desktop.authType, 'bearer');
  assert.equal(overlaid.host.desktop.actions.oauthClientId, 'actions-client');
});

test('workspace pack omits secrets and node dataDir', async () => {
  const {
    parseCanonicalWorkspace,
    exportWorkspacePack,
    parseWorkspacePack
  } = await import('../dist/workspaceDocument.js');
  const canonical = parseCanonicalWorkspace(await loadFixture('node-extras.json'));
  const pack = exportWorkspacePack(canonical, { oauthPassword: true, oauthTokenSecret: true });
  const text = JSON.stringify(pack);
  assert.equal(pack.secretPresence.oauthPassword, true);
  assert.equal(pack.host.node.dataDir, undefined);
  assert.equal(text.includes('must-not-survive'), false);
  const parsed = parseWorkspacePack({ ...pack, host: { node: { dataDir: 'C:/leak', management: { enabled: true } } } });
  assert.equal(parsed.canonical.host.node.dataDir, undefined);
  assert.equal(parsed.secretPresence.oauthPassword, true);
});
