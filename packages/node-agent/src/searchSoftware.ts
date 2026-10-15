import { execFile } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { chmod, copyFile, mkdir, mkdtemp, readdir, readFile, rm, unlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { pipeline } from 'node:stream/promises';
import { promisify } from 'node:util';
import { Readable } from 'node:stream';
import { managedRipgrepPath, resetSearchBackendCache, resolveGrepProgram, resolveSearchBackend } from './searchGrep.js';

const execFileAsync = promisify(execFile);
export const RIPGREP_VERSION = '14.1.1';
const DOWNLOAD_CONFIG_NAME = 'download-config.json';
const MAX_ARCHIVE_BYTES = 64 * 1024 * 1024;

export interface NodeSoftwareStatus {
  kind: string;
  name: string;
  installed: boolean;
  path: string;
  managed: boolean;
  group: string;
  installable: boolean;
  hint: string;
  nextSteps: string;
}

export interface NodeDownloadConfig {
  githubMirror: string;
  proxyMode: string;
  proxyUrl: string;
}

export function defaultDownloadConfig(): NodeDownloadConfig {
  return { githubMirror: '', proxyMode: 'system', proxyUrl: '' };
}

function downloadConfigPath(dataDir: string): string {
  return path.join(dataDir, DOWNLOAD_CONFIG_NAME);
}

export async function readDownloadConfig(dataDir: string): Promise<NodeDownloadConfig> {
  try {
    const parsed = JSON.parse(await readFile(downloadConfigPath(dataDir), 'utf8')) as Partial<NodeDownloadConfig>;
    return {
      githubMirror: String(parsed.githubMirror ?? ''),
      proxyMode: String(parsed.proxyMode ?? 'system'),
      proxyUrl: String(parsed.proxyUrl ?? '')
    };
  } catch {
    return defaultDownloadConfig();
  }
}

export async function writeDownloadConfig(dataDir: string, config: NodeDownloadConfig): Promise<NodeDownloadConfig> {
  const next: NodeDownloadConfig = {
    githubMirror: String(config.githubMirror ?? '').trim(),
    proxyMode: String(config.proxyMode ?? 'system').trim() || 'system',
    proxyUrl: String(config.proxyUrl ?? '').trim()
  };
  await mkdir(dataDir, { recursive: true });
  await writeFile(downloadConfigPath(dataDir), `${JSON.stringify(next, null, 2)}\n`);
  return next;
}

function samePath(left: string, right: string): boolean {
  if (!left || !right) return false;
  const normalize = (value: string): string => path.resolve(value).replaceAll('\\', '/').toLowerCase();
  return normalize(left) === normalize(right);
}

function ripgrepAsset(): { fileName: string; binaryName: string } {
  const fileName = (() => {
    if (process.platform === 'win32' && process.arch === 'x64') return `ripgrep-${RIPGREP_VERSION}-x86_64-pc-windows-msvc.zip`;
    if (process.platform === 'win32' && process.arch === 'arm64') return `ripgrep-${RIPGREP_VERSION}-aarch64-pc-windows-msvc.zip`;
    if (process.platform === 'linux' && process.arch === 'x64') return `ripgrep-${RIPGREP_VERSION}-x86_64-unknown-linux-musl.tar.gz`;
    if (process.platform === 'linux' && process.arch === 'arm64') return `ripgrep-${RIPGREP_VERSION}-aarch64-unknown-linux-gnu.tar.gz`;
    if (process.platform === 'darwin' && process.arch === 'x64') return `ripgrep-${RIPGREP_VERSION}-x86_64-apple-darwin.tar.gz`;
    if (process.platform === 'darwin' && process.arch === 'arm64') return `ripgrep-${RIPGREP_VERSION}-aarch64-apple-darwin.tar.gz`;
    throw new Error(`ripgrep has no prebuilt binary for ${process.platform}/${process.arch}. search_text will keep using Node.js search.`);
  })();
  return { fileName, binaryName: process.platform === 'win32' ? 'rg.exe' : 'rg' };
}

function githubDownloadUrls(fileName: string, config: NodeDownloadConfig): string[] {
  const official = `https://github.com/BurntSushi/ripgrep/releases/download/${RIPGREP_VERSION}/${fileName}`;
  const prefix = config.githubMirror.trim().replace(/\/$/, '');
  if (!prefix) return [official];
  return [official, `${prefix}/${official.replace(/^\//, '')}`];
}

export function listSearchSoftware(dataDir: string): NodeSoftwareStatus[] {
  resetSearchBackendCache();
  const managed = managedRipgrepPath(dataDir);
  const resolved = resolveSearchBackend(dataDir);
  const rgInstalled = resolved?.backend === 'rg';
  const rgPath = rgInstalled ? resolved.program : '';
  const rgManaged = rgInstalled && samePath(rgPath, managed);
  const grepPath = resolveGrepProgram() ?? '';
  return [
    {
      kind: 'rg',
      name: 'ripgrep (rg)',
      installed: rgInstalled,
      path: rgPath,
      managed: rgManaged,
      group: 'search',
      installable: !rgInstalled || rgManaged,
      hint: rgInstalled
        ? 'search_text uses ripgrep for workspace string search.'
        : 'Not installed. search_text currently falls back to built-in Node.js search.',
      nextSteps: ''
    },
    {
      kind: 'grep',
      name: 'GNU grep',
      installed: Boolean(grepPath),
      path: grepPath,
      managed: false,
      group: 'search',
      installable: false,
      hint: grepPath
        ? 'Used when ripgrep is missing. Install ripgrep for faster search.'
        : 'Not installed. Optional fallback after ripgrep; Windows usually does not ship grep.',
      nextSteps: ''
    }
  ];
}

async function downloadToFile(url: string, dest: string): Promise<void> {
  const response = await fetch(url, {
    redirect: 'follow',
    headers: { 'user-agent': 'coding-tools-mcp-node-agent' },
    signal: AbortSignal.timeout(180_000)
  });
  if (!response.ok || !response.body) {
    throw new Error(`Failed to download ripgrep (${response.status} ${response.statusText}).`);
  }
  const length = Number(response.headers.get('content-length') ?? 0);
  if (length > MAX_ARCHIVE_BYTES) throw new Error('ripgrep archive is larger than the allowed download limit.');
  await pipeline(Readable.fromWeb(response.body as never), createWriteStream(dest));
}

async function extractArchive(archivePath: string, destDir: string): Promise<void> {
  await mkdir(destDir, { recursive: true });
  if (archivePath.endsWith('.zip')) {
    await execFileAsync('powershell.exe', [
      '-NoProfile',
      '-NonInteractive',
      '-Command',
      'Expand-Archive -Force -LiteralPath $env:CTMCP_RG_ARCHIVE -DestinationPath $env:CTMCP_RG_DEST'
    ], {
      env: { ...process.env, CTMCP_RG_ARCHIVE: archivePath, CTMCP_RG_DEST: destDir },
      windowsHide: true,
      timeout: 60_000
    });
    return;
  }
  await execFileAsync('tar', ['-xzf', archivePath, '-C', destDir], { timeout: 60_000, windowsHide: true });
}

async function findExtractedBinary(root: string, binaryName: string, depth = 0): Promise<string | undefined> {
  if (depth > 4) return undefined;
  const entries = await readdir(root, { withFileTypes: true });
  for (const entry of entries) {
    const full = path.join(root, entry.name);
    if (entry.isFile() && entry.name === binaryName) return full;
    if (entry.isDirectory()) {
      const nested = await findExtractedBinary(full, binaryName, depth + 1);
      if (nested) return nested;
    }
  }
  return undefined;
}

export async function installRipgrep(dataDir: string, config?: NodeDownloadConfig): Promise<NodeSoftwareStatus> {
  const settings = config ?? await readDownloadConfig(dataDir);
  const asset = ripgrepAsset();
  const dest = managedRipgrepPath(dataDir);
  if (!dest) throw new Error('Agent data directory is not configured.');
  await mkdir(path.dirname(dest), { recursive: true });
  const work = await mkdtemp(path.join(os.tmpdir(), 'ctmcp-rg-'));
  try {
    const archivePath = path.join(work, asset.fileName);
    const errors: string[] = [];
    let downloaded = false;
    for (const url of githubDownloadUrls(asset.fileName, settings)) {
      try {
        await downloadToFile(url, archivePath);
        downloaded = true;
        break;
      } catch (error) {
        errors.push(`${url}: ${error instanceof Error ? error.message : String(error)}`);
      }
    }
    if (!downloaded) throw new Error(`Could not download ripgrep. ${errors.join(' ')}`);
    const extractDir = path.join(work, 'extract');
    await extractArchive(archivePath, extractDir);
    const extracted = await findExtractedBinary(extractDir, asset.binaryName);
    if (!extracted) throw new Error('ripgrep archive did not contain an rg executable.');
    await copyFile(extracted, dest);
    if (process.platform !== 'win32') await chmod(dest, 0o755);
    resetSearchBackendCache();
    const resolved = resolveSearchBackend(dataDir);
    if (resolved?.backend !== 'rg') throw new Error('ripgrep installed but could not be executed.');
    return listSearchSoftware(dataDir)[0];
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

export async function uninstallRipgrep(dataDir: string): Promise<NodeSoftwareStatus> {
  const dest = managedRipgrepPath(dataDir);
  if (!dest) throw new Error('Agent data directory is not configured.');
  try { await unlink(dest); } catch (error) {
    if (!error || typeof error !== 'object' || !('code' in error) || error.code !== 'ENOENT') throw error;
  }
  resetSearchBackendCache();
  return listSearchSoftware(dataDir)[0];
}
