import { execFileSync, spawn } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import { existsSync } from 'node:fs';
import { access, mkdir, mkdtemp, realpath, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';


const BACKEND_ID = 'wslc';
const PROVISION_TIMEOUT_MS = 90_000;

export class WslcProvisionError extends Error {
  readonly code: string;
  readonly category = 'runtime';
  readonly retryable = false;
  readonly details: Record<string, unknown>;

  constructor(code: string, message: string, stage: string, details: Record<string, unknown> = {}) {
    super(message);
    this.name = 'WslcProvisionError';
    this.code = code;
    this.details = {
      sandbox_backend: BACKEND_ID,
      stage,
      fallback_allowed: false,
      ...details
    };
  }
}

interface ProcessResult {
  code: number | null;
  stdout: string;
  stderr: string;
}

const provisioning = new Map<string, Promise<string>>();

function stripVerbatimPrefix(value: string): string {
  if (/^\\\\\?\\UNC\\/i.test(value)) return `\\\\${value.slice(8)}`;
  if (/^\\\\\?\\/.test(value)) return value.slice(4);
  return value;
}

function isNetworkPath(value: string): boolean {
  const normalized = value.replaceAll('/', '\\');
  if (/^\\\\\?\\UNC\\/i.test(normalized)) return true;
  if (/^\\\\\?\\/i.test(normalized)) return false;
  return normalized.startsWith('\\\\');
}

function comparablePath(value: string): string {
  const normalized = path.resolve(stripVerbatimPrefix(value)).replaceAll('\\', '/').replace(/\/+$/, '');
  return process.platform === 'win32' ? normalized.toLowerCase() : normalized;
}

async function pathExists(value: string): Promise<boolean> {
  try {
    await access(value);
    return true;
  } catch {
    return false;
  }
}

export function wslcProvisionerCompilerCandidates(
  windowsDirectory = process.env.WINDIR ?? 'C:\\Windows'
): string[] {
  return [
    path.join(windowsDirectory, 'Microsoft.NET', 'Framework64', 'v4.0.30319', 'csc.exe'),
    path.join(windowsDirectory, 'Microsoft.NET', 'Framework', 'v4.0.30319', 'csc.exe')
  ];
}

export function wslcHostAvailable(): boolean {
  if (process.platform !== 'win32') return false;
  const defaultCli = process.env.ProgramFiles
    ? path.join(process.env.ProgramFiles, 'WSL', 'wslc.exe')
    : undefined;
  if (defaultCli && existsSync(defaultCli)) return true;
  try {
    execFileSync('where.exe', ['wslc'], { windowsHide: true, stdio: ['ignore', 'pipe', 'ignore'] });
    return true;
  } catch {
    return false;
  }
}

export async function discoverWslcProvisionerCompiler(): Promise<string> {
  for (const candidate of wslcProvisionerCompilerCandidates()) {
    if (await pathExists(candidate)) return candidate;
  }
  throw new WslcProvisionError(
    'SANDBOX_WSLC_PROVISIONER_UNAVAILABLE',
    'WSLC managed session storage requires the Windows .NET Framework C# compiler, but csc.exe was not found.',
    'session_provisioner'
  );
}

export async function managedWslcSessionStorage(dataDir: string, workspaceRoot: string): Promise<string> {
  let workspace: string;
  try {
    workspace = await realpath(workspaceRoot);
  } catch (error) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_INVALID',
      `Cannot resolve workspace identity for WSLC session storage: ${error instanceof Error ? error.message : String(error)}`,
      'session_storage'
    );
  }
  const root = path.resolve(dataDir);
  if (isNetworkPath(root)) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_UNSUPPORTED',
      `WSLC managed session storage requires a local data directory: ${root}`,
      'session_storage'
    );
  }
  const identity = comparablePath(workspace);
  const digest = createHash('sha256').update(identity).digest('hex').slice(0, 32);
  return path.join(root, 'sandbox', 'wslc', 'sessions', digest);
}

async function validateStorage(storagePath: string): Promise<string> {
  if (isNetworkPath(storagePath)) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_UNSUPPORTED',
      `WSLC session storage cannot use a network-backed path: ${storagePath}`,
      'session_storage'
    );
  }
  let storage: string;
  try {
    storage = await realpath(storagePath);
    const [directory, vhd] = await Promise.all([
      stat(storage),
      stat(path.join(storage, 'storage.vhdx'))
    ]);
    if (!directory.isDirectory() || !vhd.isFile() || vhd.size === 0) throw new Error('storage.vhdx is missing or empty');
  } catch (error) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_INVALID',
      `WSLC session storage is invalid: ${storagePath}: ${error instanceof Error ? error.message : String(error)}`,
      'session_storage'
    );
  }
  if (isNetworkPath(storage)) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_UNSUPPORTED',
      `WSLC session storage resolves to a network-backed path: ${storage}`,
      'session_storage'
    );
  }
  return storage;
}

async function runProcess(program: string, args: string[], timeoutMs: number): Promise<ProcessResult> {
  return new Promise((resolve, reject) => {
    const child = spawn(program, args, {
      windowsHide: true,
      shell: false,
      stdio: ['ignore', 'pipe', 'pipe']
    });
    const stdout: Buffer[] = [];
    const stderr: Buffer[] = [];
    let settled = false;
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try { child.kill('SIGKILL'); } catch { /* best effort */ }
      reject(new WslcProvisionError(
        'SANDBOX_WSLC_SESSION_PROVISION_TIMEOUT',
        `WSLC session provisioning helper exceeded ${timeoutMs} ms.`,
        'session_provisioner'
      ));
    }, timeoutMs);
    timer.unref();
    child.stdout.on('data', chunk => stdout.push(Buffer.from(chunk)));
    child.stderr.on('data', chunk => stderr.push(Buffer.from(chunk)));
    child.once('error', error => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      reject(new WslcProvisionError(
        'SANDBOX_WSLC_SESSION_PROVISION_FAILED',
        `Failed to start WSLC session provisioning helper: ${error.message}`,
        'session_provisioner'
      ));
    });
    child.once('close', code => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      resolve({
        code,
        stdout: Buffer.concat(stdout).toString('utf8'),
        stderr: Buffer.concat(stderr).toString('utf8')
      });
    });
  });
}

async function provisionStorage(storagePath: string): Promise<void> {
  const compiler = await discoverWslcProvisionerCompiler();
  await mkdir(path.dirname(storagePath), { recursive: true, mode: 0o700 });
  const temporary = await mkdtemp(path.join(tmpdir(), 'ctmcp-wslc-provision-'));
  const sourcePath = path.join(temporary, 'ProvisionWslcStorage.cs');
  const helperPath = path.join(temporary, 'ProvisionWslcStorage.exe');
  const sessionName = `ctmcp-node-provision-${randomUUID().replaceAll('-', '')}`;
  try {
    await writeFile(sourcePath, WSLC_PROVISIONER_SOURCE, { encoding: 'utf8', mode: 0o600 });
    const compiled = await runProcess(compiler, [
      '/nologo',
      '/optimize+',
      '/target:exe',
      `/out:${helperPath}`,
      sourcePath
    ], 30_000);
    if (compiled.code !== 0) {
      throw new WslcProvisionError(
        'SANDBOX_WSLC_PROVISIONER_COMPILE_FAILED',
        'Failed to compile the fixed WSLC session provisioning helper.',
        'session_provisioner',
        { exit_code: compiled.code, stdout: compiled.stdout, stderr: compiled.stderr }
      );
    }
    const provisioned = await runProcess(helperPath, [sessionName, storagePath], PROVISION_TIMEOUT_MS);
    if (provisioned.code !== 0) {
      throw new WslcProvisionError(
        'SANDBOX_WSLC_SESSION_PROVISION_FAILED',
        'The WSLC session manager could not create managed session storage.',
        'session_provisioner',
        { exit_code: provisioned.code, stdout: provisioned.stdout, stderr: provisioned.stderr }
      );
    }
  } finally {
    await rm(temporary, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 }).catch(() => undefined);
  }
}

export async function ensureWslcSessionStorage(
  configuredStorage: string | undefined,
  dataDir: string,
  workspaceRoot: string
): Promise<string> {
  const configured = configuredStorage?.trim();
  if (configured && !path.isAbsolute(configured)) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_INVALID',
      `Configured WSLC session storage must use an absolute local path: ${configured}`,
      'session_storage'
    );
  }
  const target = configured
    ? path.resolve(configured)
    : await managedWslcSessionStorage(dataDir, workspaceRoot);
  if (isNetworkPath(target)) {
    throw new WslcProvisionError(
      'SANDBOX_WSLC_SESSION_STORAGE_UNSUPPORTED',
      `WSLC session storage cannot use a network-backed path: ${target}`,
      'session_storage'
    );
  }
  const key = comparablePath(target);
  const existing = provisioning.get(key);
  if (existing) return existing;

  const current = (async () => {
    if (await pathExists(target)) return validateStorage(target);
    await provisionStorage(target);
    return validateStorage(target);
  })();
  provisioning.set(key, current);
  try {
    return await current;
  } finally {
    provisioning.delete(key);
  }
}

// This source is constant product code. No command text, workspace path, image,
// environment value or user input is interpolated into it. Runtime values are
// passed to the compiled helper as ordinary argv entries.
const WSLC_PROVISIONER_SOURCE = String.raw`
using System;
using System.Runtime.InteropServices;

[StructLayout(LayoutKind.Sequential)]
public struct WslcHandle {
    public int Kind;
    public IntPtr Value;
}

[StructLayout(LayoutKind.Sequential)]
public struct WslcSessionSettings {
    public IntPtr DisplayName;
    public IntPtr StoragePath;
    public ulong MaximumStorageSizeMb;
    public uint CpuCount;
    public uint MemoryMb;
    public uint BootTimeoutMs;
    public int NetworkingMode;
    public int FeatureFlags;
    public IntPtr HostLoopback;
    public WslcHandle DmesgOutput;
    public int StorageFlags;
    public uint IdleTimeoutSec;
    public IntPtr RootVhdOverride;
    public IntPtr RootVhdTypeOverride;
}

[ComImport]
[Guid("82A7ABC8-6B50-43FC-AB96-15FBBE7E8760")]
[InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface IWslcSessionManager {
    [PreserveSig]
    int GetVersion(IntPtr version);

    [PreserveSig]
    int CreateSession(
        ref WslcSessionSettings settings,
        int flags,
        IntPtr reserved,
        [MarshalAs(UnmanagedType.Interface)] out object session);

    [PreserveSig]
    int EnterSession(
        IntPtr name,
        IntPtr storagePath,
        IntPtr reserved,
        [MarshalAs(UnmanagedType.Interface)] out object session);
}

public static class Program {
    private const uint COINIT_MULTITHREADED = 0;
    private const uint RPC_C_AUTHN_LEVEL_DEFAULT = 0;
    private const uint RPC_C_IMP_LEVEL_IMPERSONATE = 3;
    private const uint EOAC_STATIC_CLOAKING = 0x20;
    private const int RPC_E_TOO_LATE = unchecked((int)0x80010119);
    private const int RPC_E_CHANGED_MODE = unchecked((int)0x80010106);

    [DllImport("ole32.dll")]
    private static extern int CoInitializeEx(IntPtr reserved, uint coinit);

    [DllImport("ole32.dll")]
    private static extern int CoInitializeSecurity(
        IntPtr securityDescriptor,
        int authServiceCount,
        IntPtr authServices,
        IntPtr reserved1,
        uint authenticationLevel,
        uint impersonationLevel,
        IntPtr authList,
        uint capabilities,
        IntPtr reserved3);

    [DllImport("ole32.dll")]
    private static extern void CoUninitialize();

    public static int Main(string[] args) {
        if (args.Length != 2) return 2;
        int init = CoInitializeEx(IntPtr.Zero, COINIT_MULTITHREADED);
        bool uninitialize = init >= 0;
        if (init < 0 && init != RPC_E_CHANGED_MODE) return 10;
        try {
            int security = CoInitializeSecurity(
                IntPtr.Zero, -1, IntPtr.Zero, IntPtr.Zero,
                RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
                IntPtr.Zero, EOAC_STATIC_CLOAKING, IntPtr.Zero);
            if (security < 0 && security != RPC_E_TOO_LATE) return 11;

            object managerObject = null;
            object session = null;
            IntPtr display = IntPtr.Zero;
            IntPtr storage = IntPtr.Zero;
            try {
                Type managerType = Type.GetTypeFromCLSID(
                    new Guid("A9B7A1B9-0671-405C-95F1-E0612CB4CE8F"), true);
                managerObject = Activator.CreateInstance(managerType);
                var manager = (IWslcSessionManager)managerObject;
                display = Marshal.StringToHGlobalUni(args[0]);
                storage = Marshal.StringToHGlobalUni(args[1]);
                var settings = new WslcSessionSettings {
                    DisplayName = display,
                    StoragePath = storage,
                    MaximumStorageSizeMb = 32768,
                    CpuCount = 0,
                    MemoryMb = 0,
                    BootTimeoutMs = 30000,
                    NetworkingMode = 1,
                    FeatureFlags = 8,
                    HostLoopback = IntPtr.Zero,
                    DmesgOutput = new WslcHandle { Kind = 0, Value = IntPtr.Zero },
                    StorageFlags = 0,
                    IdleTimeoutSec = 0,
                    RootVhdOverride = IntPtr.Zero,
                    RootVhdTypeOverride = IntPtr.Zero
                };
                int hr = manager.CreateSession(ref settings, 0, IntPtr.Zero, out session);
                if (hr < 0) return 12;
                return 0;
            } finally {
                if (display != IntPtr.Zero) Marshal.FreeHGlobal(display);
                if (storage != IntPtr.Zero) Marshal.FreeHGlobal(storage);
                if (session != null && Marshal.IsComObject(session)) Marshal.FinalReleaseComObject(session);
                if (managerObject != null && Marshal.IsComObject(managerObject)) Marshal.FinalReleaseComObject(managerObject);
            }
        } catch (Exception error) {
            Console.Error.WriteLine(error.ToString());
            return 20;
        } finally {
            if (uninitialize) CoUninitialize();
        }
    }
}
`;
