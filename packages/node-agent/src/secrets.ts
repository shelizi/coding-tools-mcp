import { createCipheriv, createDecipheriv, randomBytes, timingSafeEqual } from 'node:crypto';
import { chmod, mkdir, readFile, rename, rm, writeFile } from 'node:fs/promises';
import path from 'node:path';
import type { AgentSecrets } from './types.js';

const STORE_VERSION = 1;
const AAD = Buffer.from('coding-tools-mcp-node/agent-secrets/v1', 'utf8');
const KEY_BYTES = 32;

export function generateAuthorizationPassword(): string {
  return randomBytes(24).toString('base64url');
}

interface SecretEnvelope {
  version: 1;
  algorithm: 'aes-256-gcm';
  iv: string;
  tag: string;
  ciphertext: string;
}

export interface SecretStoreState {
  secrets: AgentSecrets;
  storePath: string;
  keyPath: string;
  created: boolean;
  changed: boolean;
}

export interface AgentSecretFileSnapshot {
  storePath: string;
  keyPath: string;
  storeContent?: Buffer;
  keyContent?: Buffer;
}

export function secretStorePaths(dataDir: string): { storePath: string; keyPath: string } {
  return {
    storePath: path.join(dataDir, 'agent-secrets.enc.json'),
    keyPath: path.join(dataDir, 'agent-secrets.key')
  };
}

function cleanSecrets(value: AgentSecrets): AgentSecrets {
  const output: AgentSecrets = {};
  const entries: Array<[keyof AgentSecrets, unknown, number]> = [
    ['oauthPassword', value.oauthPassword, 4096],
    ['oauthClientSecret', value.oauthClientSecret, 4096],
    ['oauthTokenSecret', value.oauthTokenSecret, 4096],
    ['tunnelEnrollmentUrl', value.tunnelEnrollmentUrl, 4096]
  ];
  for (const [key, raw, maximum] of entries) {
    if (raw === undefined || raw === null || raw === '') continue;
    if (typeof raw !== 'string') throw new Error(`Secret field ${key} must be a string`);
    if (raw.length > maximum) throw new Error(`Secret field ${key} exceeds ${maximum} characters`);
    output[key] = raw;
  }
  if (output.oauthTokenSecret !== undefined && !output.oauthTokenSecret.trim()) {
    throw new Error('OAuth token secret is not configured');
  }
  return output;
}

function sameSecrets(left: AgentSecrets, right: AgentSecrets): boolean {
  const leftBytes = Buffer.from(JSON.stringify(cleanSecrets(left)));
  const rightBytes = Buffer.from(JSON.stringify(cleanSecrets(right)));
  return leftBytes.length === rightBytes.length && timingSafeEqual(leftBytes, rightBytes);
}

async function readKey(keyPath: string): Promise<Buffer | undefined> {
  try {
    const encoded = (await readFile(keyPath, 'utf8')).trim();
    const key = Buffer.from(encoded, 'base64url');
    if (key.length !== KEY_BYTES) throw new Error(`Agent secret key is invalid: ${keyPath}`);
    await chmod(keyPath, 0o600).catch(() => undefined);
    return key;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

async function loadOrCreateKey(dataDir: string): Promise<{ key: Buffer; keyPath: string }> {
  const { keyPath } = secretStorePaths(dataDir);
  const existing = await readKey(keyPath);
  if (existing) return { key: existing, keyPath };
  await mkdir(dataDir, { recursive: true, mode: 0o700 });
  const generated = randomBytes(KEY_BYTES);
  try {
    await writeFile(keyPath, `${generated.toString('base64url')}\n`, { flag: 'wx', mode: 0o600 });
    return { key: generated, keyPath };
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== 'EEXIST') throw error;
    const raced = await readKey(keyPath);
    if (!raced) throw new Error(`Agent secret key disappeared after concurrent creation: ${keyPath}`);
    return { key: raced, keyPath };
  }
}

function encryptSecrets(secrets: AgentSecrets, key: Buffer): SecretEnvelope {
  const iv = randomBytes(12);
  const cipher = createCipheriv('aes-256-gcm', key, iv);
  cipher.setAAD(AAD);
  const ciphertext = Buffer.concat([
    cipher.update(JSON.stringify(cleanSecrets(secrets)), 'utf8'),
    cipher.final()
  ]);
  return {
    version: STORE_VERSION,
    algorithm: 'aes-256-gcm',
    iv: iv.toString('base64url'),
    tag: cipher.getAuthTag().toString('base64url'),
    ciphertext: ciphertext.toString('base64url')
  };
}

function decryptSecrets(envelope: SecretEnvelope, key: Buffer, storePath: string): AgentSecrets {
  if (envelope.version !== STORE_VERSION || envelope.algorithm !== 'aes-256-gcm') {
    throw new Error(`Unsupported agent secret store format: ${storePath}`);
  }
  try {
    const decipher = createDecipheriv('aes-256-gcm', key, Buffer.from(envelope.iv, 'base64url'));
    decipher.setAAD(AAD);
    decipher.setAuthTag(Buffer.from(envelope.tag, 'base64url'));
    const plaintext = Buffer.concat([
      decipher.update(Buffer.from(envelope.ciphertext, 'base64url')),
      decipher.final()
    ]);
    return cleanSecrets(JSON.parse(plaintext.toString('utf8')) as AgentSecrets);
  } catch {
    throw new Error(`Unable to decrypt agent secret store: ${storePath}`);
  }
}

async function readStore(storePath: string, key: Buffer): Promise<AgentSecrets | undefined> {
  try {
    const envelope = JSON.parse(await readFile(storePath, 'utf8')) as SecretEnvelope;
    await chmod(storePath, 0o600).catch(() => undefined);
    return decryptSecrets(envelope, key, storePath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

async function writeAtomic(filePath: string, content: string): Promise<void> {
  await mkdir(path.dirname(filePath), { recursive: true, mode: 0o700 });
  const temporary = `${filePath}.${process.pid}.${randomBytes(6).toString('hex')}.tmp`;
  await writeFile(temporary, content, { flag: 'wx', mode: 0o600 });
  try {
    await rename(temporary, filePath);
  } catch (error) {
    await rm(temporary, { force: true }).catch(() => undefined);
    throw error;
  }
  await chmod(filePath, 0o600).catch(() => undefined);
}

async function readOptionalFile(filePath: string): Promise<Buffer | undefined> {
  try {
    return await readFile(filePath);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined;
    throw error;
  }
}

export async function snapshotAgentSecretFiles(dataDir: string): Promise<AgentSecretFileSnapshot> {
  const paths = secretStorePaths(dataDir);
  const [storeContent, keyContent] = await Promise.all([
    readOptionalFile(paths.storePath),
    readOptionalFile(paths.keyPath)
  ]);
  return { ...paths, storeContent, keyContent };
}

export async function restoreAgentSecretFiles(snapshot: AgentSecretFileSnapshot): Promise<void> {
  if (snapshot.keyContent) await writeAtomic(snapshot.keyPath, snapshot.keyContent.toString('utf8'));
  else await rm(snapshot.keyPath, { force: true });
  if (snapshot.storeContent) await writeAtomic(snapshot.storePath, snapshot.storeContent.toString('utf8'));
  else await rm(snapshot.storePath, { force: true });
}

async function createStoreExclusive(storePath: string, secrets: AgentSecrets, key: Buffer): Promise<boolean> {
  const content = `${JSON.stringify(encryptSecrets(secrets, key), null, 2)}\n`;
  try {
    await writeFile(storePath, content, { flag: 'wx', mode: 0o600 });
    return true;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === 'EEXIST') return false;
    throw error;
  }
}

export async function readAgentSecrets(dataDir: string): Promise<SecretStoreState> {
  const paths = secretStorePaths(dataDir);
  const key = await readKey(paths.keyPath);
  if (!key) {
    try {
      await readFile(paths.storePath, 'utf8');
      throw new Error(`Agent secret store exists without its key: ${paths.storePath}`);
    } catch (error) {
      if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error;
    }
    return { secrets: {}, ...paths, created: false, changed: false };
  }
  return {
    secrets: await readStore(paths.storePath, key) ?? {},
    ...paths,
    created: false,
    changed: false
  };
}

export async function ensureAgentSecrets(
  dataDir: string,
  seed: AgentSecrets = {},
  generateTokenSecret = true
): Promise<SecretStoreState> {
  const paths = secretStorePaths(dataDir);
  const initial = cleanSecrets(seed);
  let existingState = await readAgentSecrets(dataDir);
  let merged = cleanSecrets({ ...initial, ...existingState.secrets });
  if (generateTokenSecret && !merged.oauthPassword) merged.oauthPassword = generateAuthorizationPassword();
  if (generateTokenSecret && !merged.oauthTokenSecret) merged.oauthTokenSecret = randomBytes(32).toString('hex');
  if (Object.keys(merged).length === 0) return existingState;
  if (sameSecrets(existingState.secrets, merged)) return { ...existingState, secrets: merged };

  const { key, keyPath } = await loadOrCreateKey(dataDir);
  if (Object.keys(existingState.secrets).length === 0) {
    const created = await createStoreExclusive(paths.storePath, merged, key);
    if (created) return { secrets: merged, storePath: paths.storePath, keyPath, created: true, changed: true };
    existingState = {
      secrets: await readStore(paths.storePath, key) ?? {},
      storePath: paths.storePath,
      keyPath,
      created: false,
      changed: false
    };
    merged = cleanSecrets({ ...initial, ...existingState.secrets });
    if (generateTokenSecret && !merged.oauthPassword) merged.oauthPassword = generateAuthorizationPassword();
    if (generateTokenSecret && !merged.oauthTokenSecret) merged.oauthTokenSecret = randomBytes(32).toString('hex');
    if (sameSecrets(existingState.secrets, merged)) return { ...existingState, secrets: merged };
  }

  await writeAtomic(paths.storePath, `${JSON.stringify(encryptSecrets(merged, key), null, 2)}\n`);
  return { secrets: merged, storePath: paths.storePath, keyPath, created: false, changed: true };
}

export async function writeAgentSecrets(dataDir: string, secrets: AgentSecrets): Promise<SecretStoreState> {
  const cleaned = cleanSecrets(secrets);
  if (Object.keys(cleaned).length === 0) {
    const existing = await readAgentSecrets(dataDir);
    if (Object.keys(existing.secrets).length === 0) return existing;
  }
  const { key, keyPath } = await loadOrCreateKey(dataDir);
  const { storePath } = secretStorePaths(dataDir);
  const previous = await readStore(storePath, key) ?? {};
  if (sameSecrets(previous, cleaned)) {
    return { secrets: cleaned, storePath, keyPath, created: false, changed: false };
  }
  await writeAtomic(storePath, `${JSON.stringify(encryptSecrets(cleaned, key), null, 2)}\n`);
  return { secrets: cleaned, storePath, keyPath, created: Object.keys(previous).length === 0, changed: true };
}
