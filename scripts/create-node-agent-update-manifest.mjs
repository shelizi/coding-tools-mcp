#!/usr/bin/env node
import { readFile, writeFile } from 'node:fs/promises';
import { basename } from 'node:path';
import { pathToFileURL } from 'node:url';

import { signUpdateMetadata } from './sign-node-agent-update.mjs';

const TAG_RE = /^node-agent-v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)-portable-v(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/;
const SHA_RE = /^[0-9a-f]{40}$/;

function parseArgs(argv) {
  const values = new Map();
  for (let index = 0; index < argv.length; index += 2) {
    const key = argv[index];
    const value = argv[index + 1];
    if (!key?.startsWith('--') || !value) throw new Error(`invalid argument near ${key ?? '<end>'}`);
    values.set(key.slice(2), value);
  }
  for (const required of ['release-tag', 'git-commit', 'bundled', 'system', 'private-key', 'output']) {
    if (!values.has(required)) throw new Error(`--${required} is required`);
  }
  return values;
}

export function versionsFromReleaseTag(releaseTag) {
  const match = TAG_RE.exec(releaseTag);
  if (!match) throw new Error('release tag must be node-agent-vX.Y.Z-portable-vA.B.C');
  return {
    agentVersion: `${match[1]}.${match[2]}.${match[3]}`,
    portableVersion: `${match[4]}.${match[5]}.${match[6]}`
  };
}

export function createReleaseManifest({ releaseTag, gitCommit, artifacts, privateKeyPem }) {
  const { agentVersion, portableVersion } = versionsFromReleaseTag(releaseTag);
  if (!SHA_RE.test(gitCommit)) throw new Error('git commit must be a full lowercase 40-character SHA');
  const expected = [
    ['bundled-node', `ctnode-${agentVersion}-p${portableVersion}-win64.zip`],
    ['system-node', `ctnode-${agentVersion}-p${portableVersion}-sys-win64.zip`]
  ];
  const entries = expected.map(([edition, expectedName]) => {
    const artifact = artifacts.find(item => item.edition === edition);
    if (!artifact) throw new Error(`missing ${edition} artifact`);
    if (basename(artifact.path) !== expectedName) {
      throw new Error(`${edition} filename must be ${expectedName}`);
    }
    const signed = signUpdateMetadata({
      artifact: artifact.bytes,
      version: agentVersion,
      url: `https://release.invalid/${encodeURIComponent(expectedName)}`,
      privateKeyPem
    });
    return {
      edition,
      platform: 'win32',
      arch: 'x64',
      filename: expectedName,
      sha256: signed.sha256,
      signature: signed.signature
    };
  });
  return {
    manifest: {
      schemaVersion: 1,
      releaseTag,
      agentVersion,
      portableVersion,
      gitCommit,
      artifacts: entries
    },
    trustedPublicKey: signUpdateMetadata({
      artifact: artifacts[0].bytes,
      version: agentVersion,
      url: 'https://release.invalid/key-check',
      privateKeyPem
    }).trustedPublicKey
  };
}

export async function main(argv = process.argv.slice(2)) {
  const args = parseArgs(argv);
  const releaseTag = args.get('release-tag');
  const gitCommit = args.get('git-commit');
  const [bundledBytes, systemBytes, privateKeyPem] = await Promise.all([
    readFile(args.get('bundled')),
    readFile(args.get('system')),
    readFile(args.get('private-key'), 'utf8')
  ]);
  const { manifest, trustedPublicKey } = createReleaseManifest({
    releaseTag,
    gitCommit,
    artifacts: [
      { edition: 'bundled-node', path: args.get('bundled'), bytes: bundledBytes },
      { edition: 'system-node', path: args.get('system'), bytes: systemBytes }
    ],
    privateKeyPem
  });
  await writeFile(args.get('output'), `${JSON.stringify(manifest, null, 2)}\n`, { encoding: 'utf8', flag: 'wx' });
  process.stdout.write(`Node update manifest: ${args.get('output')}\n`);
  process.stdout.write(`Trusted public key: ${trustedPublicKey}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch(error => {
    process.stderr.write(`create-node-agent-update-manifest: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
