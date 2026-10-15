#!/usr/bin/env node
import { readFile } from 'node:fs/promises';
import { basename } from 'node:path';
import { pathToFileURL } from 'node:url';

const TAG_RE = /^node-agent-v\d+\.\d+\.\d+-portable-v\d+\.\d+\.\d+$/;

export function parseCliArgs(argv) {
  const values = new Map();
  const assetPaths = [];
  for (let index = 0; index < argv.length;) {
    const key = argv[index];
    if (!key?.startsWith('--')) throw new Error(`invalid argument near ${key ?? '<end>'}`);
    const value = argv[index + 1];
    if (!value || value.startsWith('--')) throw new Error(`${key} requires a value`);
    const name = key.slice(2);
    if (name === 'asset') assetPaths.push(value);
    else if (values.has(name)) throw new Error(`duplicate --${name}`);
    else values.set(name, value);
    index += 2;
  }
  for (const required of ['gitea-url', 'repository', 'tag']) {
    if (!values.has(required)) throw new Error(`--${required} is required`);
  }
  if (assetPaths.length !== 3) throw new Error('exactly three --asset values are required');
  return { values, assetPaths };
}

export function releaseApiBase(giteaUrl, repository) {
  const base = new URL(giteaUrl);
  if (base.protocol !== 'https:' || base.username || base.password || base.search || base.hash) {
    throw new Error('Gitea URL must be credential-free HTTPS');
  }
  const parts = repository.split('/');
  if (parts.length !== 2 || parts.some(part => !/^[0-9A-Za-z._-]{1,128}$/.test(part))) {
    throw new Error('repository must be owner/repository');
  }
  base.pathname = `${base.pathname.replace(/\/$/, '')}/api/v1/repos/${encodeURIComponent(parts[0])}/${encodeURIComponent(parts[1])}/releases`;
  return base;
}

export function releaseAssetNames(assetPaths) {
  return assetPaths.map(assetPath => basename(assetPath));
}

async function api(fetchImpl, url, token, options = {}, accepted = [200]) {
  const headers = new Headers(options.headers ?? {});
  headers.set('Authorization', `token ${token}`);
  headers.set('Accept', 'application/json');
  const response = await fetchImpl(url, { ...options, headers, redirect: 'error' });
  if (!accepted.includes(response.status)) {
    const body = await response.text();
    throw new Error(`Gitea API ${response.status}: ${body.slice(0, 500)}`);
  }
  if (response.status === 204) return null;
  return response.json();
}

export async function publishRelease({ giteaUrl, repository, tag, assetPaths, token, fetchImpl = fetch }) {
  if (!TAG_RE.test(tag)) throw new Error('invalid Node Agent portable release tag');
  if (!token || /[\r\n]/.test(token)) throw new Error('GITEA_TOKEN is missing or invalid');
  const base = releaseApiBase(giteaUrl, repository);
  const tagUrl = new URL(`${base.toString()}/tags/${encodeURIComponent(tag)}`);
  const existingResponse = await fetchImpl(tagUrl, {
    headers: { Authorization: `token ${token}`, Accept: 'application/json' },
    redirect: 'error'
  });
  let release;
  if (existingResponse.status === 404) {
    release = await api(fetchImpl, base, token, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ tag_name: tag, name: tag, draft: true, prerelease: false })
    }, [201]);
  } else if (existingResponse.status === 200) {
    release = await existingResponse.json();
    if (!release.draft) throw new Error(`release ${tag} is already published; refusing to mutate immutable assets`);
  } else {
    const body = await existingResponse.text();
    throw new Error(`Gitea API ${existingResponse.status}: ${body.slice(0, 500)}`);
  }

  const names = releaseAssetNames(assetPaths);
  if (new Set(names).size !== names.length) throw new Error('release asset filenames must be unique');
  for (const asset of release.assets ?? []) {
    if (!names.includes(asset.name)) continue;
    await api(fetchImpl, new URL(`${base.toString()}/${release.id}/assets/${asset.id}`), token, { method: 'DELETE' }, [204]);
  }

  for (const assetPath of assetPaths) {
    const bytes = await readFile(assetPath);
    const form = new FormData();
    form.append('attachment', new Blob([bytes]), basename(assetPath));
    const uploadUrl = new URL(`${base.toString()}/${release.id}/assets`);
    uploadUrl.searchParams.set('name', basename(assetPath));
    await api(fetchImpl, uploadUrl, token, { method: 'POST', body: form }, [201]);
  }

  release = await api(fetchImpl, new URL(`${base.toString()}/${release.id}`), token, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ tag_name: tag, name: tag, draft: false, prerelease: false })
  }, [200]);
  return release;
}

export async function main(argv = process.argv.slice(2)) {
  const { values: args, assetPaths } = parseCliArgs(argv);
  const token = process.env.GITEA_TOKEN;
  const release = await publishRelease({
    giteaUrl: args.get('gitea-url'),
    repository: args.get('repository'),
    tag: args.get('tag'),
    assetPaths,
    token
  });
  process.stdout.write(`Published Gitea release ${release.tag_name} (${release.id})\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1] ?? '').href) {
  main().catch(error => {
    process.stderr.write(`publish-gitea-node-agent-release: ${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
