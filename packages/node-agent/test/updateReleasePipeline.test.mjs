import assert from 'node:assert/strict';
import { generateKeyPairSync } from 'node:crypto';
import { readFileSync } from 'node:fs';
import test from 'node:test';

import { createReleaseManifest, versionsFromReleaseTag } from '../../../scripts/create-node-agent-update-manifest.mjs';
import { parseCliArgs, publishRelease, releaseApiBase, releaseAssetNames } from '../../../scripts/publish-gitea-node-agent-release.mjs';
import { verifyNodeUpdateArtifact } from '../dist/liveUpdate.js';

function signingKey() {
  const { privateKey } = generateKeyPairSync('ed25519');
  return privateKey.export({ format: 'pem', type: 'pkcs8' });
}

test('release manifest signs both portable editions with the Node Agent trust contract', () => {
  const privateKeyPem = signingKey();
  const bundled = Buffer.from('bundled-release');
  const system = Buffer.from('system-release');
  const { manifest, trustedPublicKey } = createReleaseManifest({
    releaseTag: 'node-agent-v0.29.35-portable-v2.0.2',
    gitCommit: 'a'.repeat(40),
    artifacts: [
      { edition: 'bundled-node', path: 'ctnode-0.29.35-p2.0.2-win64.zip', bytes: bundled },
      { edition: 'system-node', path: 'ctnode-0.29.35-p2.0.2-sys-win64.zip', bytes: system }
    ],
    privateKeyPem
  });
  assert.equal(manifest.schemaVersion, 1);
  assert.equal(manifest.artifacts.length, 2);
  for (const [artifact, bytes] of [[manifest.artifacts[0], bundled], [manifest.artifacts[1], system]]) {
    const offer = {
      update_id: `release-${artifact.edition}`,
      version: manifest.agentVersion,
      url: `https://updates.example.test/${artifact.filename}`,
      sha256: artifact.sha256,
      signature: artifact.signature
    };
    assert.deepEqual(verifyNodeUpdateArtifact(offer, bytes, trustedPublicKey), offer);
  }
});

test('release manifest rejects tag and filename identity mismatches', () => {
  assert.deepEqual(versionsFromReleaseTag('node-agent-v1.2.3-portable-v4.5.6'), {
    agentVersion: '1.2.3', portableVersion: '4.5.6'
  });
  assert.throws(() => versionsFromReleaseTag('v1.2.3'), /release tag/);
  assert.throws(() => createReleaseManifest({
    releaseTag: 'node-agent-v0.29.35-portable-v2.0.2',
    gitCommit: 'a'.repeat(40),
    artifacts: [
      { edition: 'bundled-node', path: 'wrong.zip', bytes: Buffer.from('a') },
      { edition: 'system-node', path: 'ctnode-0.29.35-p2.0.2-sys-win64.zip', bytes: Buffer.from('b') }
    ],
    privateKeyPem: signingKey()
  }), /filename/);
});

test('Gitea release workflow keeps signing key on the dedicated runner', () => {
  const workflow = readFileSync(new URL('../../../.gitea/workflows/publish-tunnel-server.yml', import.meta.url), 'utf8');
  assert.match(workflow, /runs-on: node-agent-release/);
  assert.match(workflow, /Restore annotated release tag/);
  assert.match(workflow, /git fetch --force origin/);
  assert.match(workflow, /git cat-file -t/);
  assert.match(workflow, /NODE_AGENT_UPDATE_PRIVATE_KEY_FILE/);
  assert.match(workflow, /Built-in GITEA_TOKEN is required for release publication/);
  assert.match(workflow, /Gitea release API preflight/);
  assert.match(workflow, /Copy-Item -LiteralPath \$env:NODE_AGENT_UPDATE_PRIVATE_KEY_FILE/);
  assert.match(workflow, /Setup isolated pnpm 12/);
  assert.match(workflow, /npm install --global --prefix \$pnpmHome pnpm@12\.0\.0/);
  assert.match(workflow, /Join-Path \$pnpmHome 'pnpm\.cmd'/);
  assert.match(workflow, /& \$pnpmCmd --version/);
  assert.equal([...workflow.matchAll(/\$env:PATH = "\$pnpmHome;\$env:PATH"/g)].length, 3);
  assert.match(workflow, /& \$pnpmCmd install --frozen-lockfile/);
  assert.match(workflow, /& \$pnpmCmd --dir packages\/node-agent run test:full:unlocked/);
  assert.doesNotMatch(workflow, /\$env:GITHUB_PATH/);
  assert.doesNotMatch(workflow, /corepack enable/);
  assert.doesNotMatch(workflow, /secrets\.NODE_AGENT_UPDATE_PRIVATE_KEY/);
  assert.match(workflow, /\$publishArgs = @\(/);
  assert.equal([...workflow.matchAll(/'--asset'/g)].length, 3);
  assert.match(workflow, /& node @publishArgs/);
  assert.doesNotMatch(workflow, /steps\.manifest\.outputs/);
  assert.equal([...workflow.matchAll(/\$tag = \$env:SOURCE_REF\.Substring\('refs\/tags\/'\.Length\)/g)].length, 3);
  assert.match(workflow, /'--tag', \$tag/);
  assert.match(workflow, /'--asset', \$bundled/);
  assert.match(workflow, /'--asset', \$system/);
  assert.match(workflow, /'--asset', \$manifest/);
});

test('Gitea publisher derives release asset names without passing map indexes as basename suffixes', () => {
  assert.deepEqual(releaseAssetNames([
    'release/ctnode-0.29.42-p2.0.1-win64.zip',
    'release/ctnode-0.29.42-p2.0.1-sys-win64.zip',
    'release/node-agent-update.json'
  ]), [
    'ctnode-0.29.42-p2.0.1-win64.zip',
    'ctnode-0.29.42-p2.0.1-sys-win64.zip',
    'node-agent-update.json'
  ]);
});

test('Gitea publisher CLI preserves repeated Windows asset paths', () => {
  const { values, assetPaths } = parseCliArgs([
    '--gitea-url', 'https://git.example.test',
    '--repository', 'neo/coding-tools-mcp',
    '--tag', 'node-agent-v0.29.40-portable-v2.0.1',
    '--asset', 'C:\\release dir\\ctnode-0.29.40-p2.0.1-win64.zip',
    '--asset', 'C:\\release dir\\ctnode-0.29.40-p2.0.1-sys-win64.zip',
    '--asset', 'C:\\release dir\\node-agent-update.json'
  ]);
  assert.equal(values.get('tag'), 'node-agent-v0.29.40-portable-v2.0.1');
  assert.deepEqual(assetPaths, [
    'C:\\release dir\\ctnode-0.29.40-p2.0.1-win64.zip',
    'C:\\release dir\\ctnode-0.29.40-p2.0.1-sys-win64.zip',
    'C:\\release dir\\node-agent-update.json'
  ]);
  assert.throws(() => parseCliArgs([
    '--gitea-url', 'https://git.example.test',
    '--repository', 'neo/coding-tools-mcp',
    '--tag', 'node-agent-v0.29.40-portable-v2.0.1',
    '--asset', 'one.zip',
    '--asset', 'two.zip'
  ]), /exactly three/);
});

test('Gitea publisher constructs a scoped API URL and refuses to mutate a published release', async () => {
  assert.equal(
    releaseApiBase('https://git.example.test/', 'neo/coding-tools-mcp').toString(),
    'https://git.example.test/api/v1/repos/neo/coding-tools-mcp/releases'
  );
  assert.throws(() => releaseApiBase('http://git.example.test', 'neo/repo'), /HTTPS/);
  let calls = 0;
  const fetchImpl = async () => {
    calls += 1;
    return new Response(JSON.stringify({ id: 7, tag_name: 'node-agent-v0.29.35-portable-v2.0.2', draft: false }), {
      status: 200, headers: { 'content-type': 'application/json' }
    });
  };
  await assert.rejects(() => publishRelease({
    giteaUrl: 'https://git.example.test',
    repository: 'neo/coding-tools-mcp',
    tag: 'node-agent-v0.29.35-portable-v2.0.2',
    assetPaths: ['a.zip', 'b.zip', 'node-agent-update.json'],
    token: 'token-value',
    fetchImpl
  }), /already published/);
  assert.equal(calls, 1);
});
