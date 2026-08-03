import { readFile, writeFile } from 'node:fs/promises';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const scriptPath = fileURLToPath(import.meta.url);
const workspace = resolve(dirname(scriptPath), '..');
const versionPattern = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

export function assertVersion(version) {
  if (typeof version !== 'string' || !versionPattern.test(version)) {
    throw new Error(`package.json contains an invalid semantic version: ${String(version)}`);
  }
  return version;
}

export function syncPackageLock(text, version) {
  const lock = JSON.parse(text);
  if (!lock.packages?.['']) {
    throw new Error('package-lock.json does not contain the root package entry.');
  }
  if (lock.version === version && lock.packages[''].version === version) {
    return text;
  }

  lock.version = version;
  lock.packages[''].version = version;
  const newline = text.includes('\r\n') ? '\r\n' : '\n';
  return `${JSON.stringify(lock, null, 2).replaceAll('\n', newline)}${newline}`;
}

function replaceRequired(text, pattern, replacement, fileName) {
  if (!pattern.test(text)) {
    throw new Error(`${fileName} does not contain the expected desktop package version.`);
  }
  pattern.lastIndex = 0;
  return text.replace(pattern, replacement);
}

export function syncCargoManifest(text, version) {
  return replaceRequired(
    text,
    /^(\[package\]\r?\nname\s*=\s*"coding-tools-mcp-desktop"\r?\nversion\s*=\s*")[^"]+("\s*)$/m,
    `$1${version}$2`,
    'src-tauri/Cargo.toml'
  );
}

export function syncCargoLock(text, version) {
  return replaceRequired(
    text,
    /^(\[\[package\]\]\r?\nname = "coding-tools-mcp-desktop"\r?\nversion = ")[^"]+("\s*)$/m,
    `$1${version}$2`,
    'src-tauri/Cargo.lock'
  );
}

async function loadJson(path) {
  return JSON.parse(await readFile(path, 'utf8'));
}

async function main() {
  const checkOnly = process.argv.slice(2).includes('--check');
  const packageJsonPath = join(workspace, 'package.json');
  const packageJson = await loadJson(packageJsonPath);
  const version = assertVersion(packageJson.version);

  const tauriConfig = await loadJson(join(workspace, 'src-tauri', 'tauri.conf.json'));
  if (tauriConfig.version !== '../package.json') {
    throw new Error('src-tauri/tauri.conf.json must use ../package.json as its version source.');
  }

  const targets = [
    {
      path: join(workspace, 'package-lock.json'),
      sync: syncPackageLock
    },
    {
      path: join(workspace, 'src-tauri', 'Cargo.toml'),
      sync: syncCargoManifest
    },
    {
      path: join(workspace, 'src-tauri', 'Cargo.lock'),
      sync: syncCargoLock
    }
  ];

  const stale = [];
  for (const target of targets) {
    const current = await readFile(target.path, 'utf8');
    const synchronized = target.sync(current, version);
    if (synchronized === current) continue;
    stale.push(target.path);
    if (!checkOnly) await writeFile(target.path, synchronized, 'utf8');
  }

  if (checkOnly && stale.length > 0) {
    throw new Error(
      `Version metadata is out of sync with package.json (${version}):\n${stale.map((path) => `- ${path}`).join('\n')}\nRun npm run version:sync.`
    );
  }

  const action = checkOnly ? 'verified' : 'synchronized';
  console.log(`Version ${version} ${action}; package.json is the source of truth.`);
}

if (process.argv[1] && resolve(process.argv[1]) === scriptPath) {
  main().catch((error) => {
    console.error(error.message);
    process.exitCode = 1;
  });
}
