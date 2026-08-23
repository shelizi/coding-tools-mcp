#[cfg(windows)]
use std::collections::BTreeMap;
#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::fs::{self, File};
#[cfg(windows)]
use std::io::{Read, Write};
#[cfg(windows)]
use std::mem;
#[cfg(windows)]
use std::os::windows::io::{FromRawHandle, RawHandle};
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use std::thread;
#[cfg(windows)]
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use windows::core::{HSTRING, PCWSTR, PWSTR};
#[cfg(windows)]
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, SetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT, HLOCAL,
    WAIT_OBJECT_0,
};
#[cfg(windows)]
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
#[cfg(windows)]
use windows::Win32::Security::{
    DeriveCapabilitySidsFromName, FreeSid, GetTokenInformation, TokenIsAppContainer, PSID,
    SECURITY_ATTRIBUTES, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows::Win32::System::Pipes::CreatePipe;
#[cfg(windows)]
use windows::Win32::System::SystemServices::SE_GROUP_ENABLED;
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, ResumeThread, UpdateProcThreadAttribute,
    WaitForSingleObject, CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT,
    EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST, PROCESS_INFORMATION,
    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTF_USESTDHANDLES, STARTUPINFOEXW,
};

#[cfg(windows)]
const CHILD_EXIT: u32 = 23;

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("appcontainer broker probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    println!("AppContainer broker probe is Windows-only");
}

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.get(1).is_some_and(|value| value == "--broker-child") {
        return run_broker_child();
    }
    if args.get(1).is_some_and(|value| value == "--sleep-child") {
        thread::sleep(Duration::from_secs(30));
        return Ok(());
    }
    if args
        .get(1)
        .is_some_and(|value| value == "--toolchain-matrix")
    {
        return run_toolchain_matrix();
    }
    if args
        .get(1)
        .is_some_and(|value| value == "--delete-matrix-profiles-only")
    {
        return delete_matrix_profiles_only();
    }
    if args
        .get(1)
        .is_some_and(|value| value == "--runtime-capability-matrix")
    {
        return run_runtime_capability_matrix();
    }
    if args
        .get(1)
        .is_some_and(|value| value == "--cleanup-runtime-capability-grants")
    {
        return cleanup_runtime_capability_grants();
    }
    if args
        .get(1)
        .is_some_and(|value| value == "--state-env-matrix")
    {
        return run_state_environment_matrix();
    }
    run_parent_probe()
}

#[cfg(windows)]
fn run_broker_child() -> Result<(), Box<dyn std::error::Error>> {
    let is_appcontainer = is_current_process_appcontainer()?;
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    print!("stdout:{}", input);
    std::io::stdout().flush()?;
    eprintln!("stderr:appcontainer={is_appcontainer}");
    std::process::exit(if is_appcontainer {
        CHILD_EXIT as i32
    } else {
        24
    });
}

#[cfg(windows)]
fn run_parent_probe() -> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let moniker = format!(
        "CodingToolsMcp.Sandbox.Broker.{}.{}",
        std::process::id(),
        nonce
    );
    let root = std::env::temp_dir().join(format!("ctmcp-appcontainer-broker-{nonce}"));
    let inside = root.join("inside");
    fs::create_dir_all(&inside)?;
    let child_exe = inside.join("sandbox-broker-child.exe");
    fs::copy(std::env::current_exe()?, &child_exe)?;

    let profile = AppContainerProfile::create(&moniker, None)?;
    let sid_string = sid_string(profile.sid)?;
    grant_acl(&root, &sid_string, "(OI)(CI)RX")?;
    grant_acl(&inside, &sid_string, "(OI)(CI)M")?;

    let stdio = run_stdio_roundtrip(&child_exe, &inside, profile.sid)?;
    let job_kill = run_job_kill_probe(&child_exe, &inside, profile.sid)?;

    println!("moniker={moniker}");
    println!("sid={sid_string}");
    println!("stdio_child_exit_code={}", stdio.exit_code);
    println!("stdout={:?}", stdio.stdout);
    println!("stderr={:?}", stdio.stderr);
    println!("job_assigned={}", stdio.job_assigned);
    println!("job_kill_terminated={job_kill}");

    let passed = stdio.exit_code == CHILD_EXIT
        && stdio.stdout == "stdout:broker-ping\n"
        && stdio.stderr.contains("stderr:appcontainer=true")
        && stdio.job_assigned
        && job_kill;

    drop(profile);
    let _ = fs::remove_dir_all(&root);
    if !passed {
        return Err("broker stdio/job contract failed".into());
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Debug)]
struct ToolchainResult {
    name: String,
    executable: PathBuf,
    default_ok: bool,
    grant_root: Option<PathBuf>,
    final_exit_code: Option<u32>,
    stdout: String,
    stderr: String,
    launch_error: Option<String>,
}

#[cfg(windows)]
fn matrix_profile_mappings() -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let output = Command::new("reg.exe")
        .args([
            "query",
            r"HKCU\Software\Classes\Local Settings\Software\Microsoft\Windows\CurrentVersion\AppContainer\Mappings",
            "/s",
            "/f",
            "CodingToolsMcp.Sandbox.Matrix",
        ])
        .output()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut mappings = Vec::new();
    let mut current_sid: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(index) = trimmed.rfind(r"\Mappings\S-1-15-2-") {
            current_sid = Some(trimmed[index + "\\Mappings\\".len()..].to_string());
            continue;
        }
        if trimmed.to_ascii_lowercase().starts_with("moniker") {
            let moniker = trimmed
                .split_whitespace()
                .last()
                .unwrap_or_default()
                .to_string();
            if moniker
                .to_ascii_lowercase()
                .starts_with("codingtoolsmcp.sandbox.matrix.")
            {
                if let Some(sid) = current_sid.take() {
                    mappings.push((sid, moniker));
                }
            }
        }
    }
    Ok(mappings)
}

#[cfg(windows)]
fn delete_matrix_profiles_only() -> Result<(), Box<dyn std::error::Error>> {
    let mappings = matrix_profile_mappings()?;
    println!("matrix_profiles_before={}", mappings.len());
    for (sid, moniker) in mappings {
        unsafe { DeleteAppContainerProfile(&HSTRING::from(&moniker))? };
        println!("deleted_profile moniker={moniker} sid={sid}");
    }
    println!("matrix_profiles_after={}", matrix_profile_mappings()?.len());
    Ok(())
}

#[cfg(windows)]
const RUNTIME_CAPABILITY_NAME: &str = "CodingToolsMcp.Sandbox.RuntimeProbe";

#[cfg(windows)]
struct RuntimeCapabilityRoots {
    python: PathBuf,
    python_root: PathBuf,
    python_physical_root: PathBuf,
    rustc: PathBuf,
    cargo: PathBuf,
    rust_bin: PathBuf,
    rust_lib: PathBuf,
    node_root: PathBuf,
    node_physical_root: PathBuf,
    node_modules: PathBuf,
    npm_root: PathBuf,
}

#[cfg(windows)]
fn runtime_capability_roots() -> Result<RuntimeCapabilityRoots, Box<dyn std::error::Error>> {
    let python = preferred_python()?;
    let python_root = python
        .parent()
        .ok_or("python executable has no parent")?
        .to_path_buf();
    let python_physical_root = resolve_reparse_target(&python_root)?;
    let rustc = rustup_which("rustc").ok_or("rustup could not resolve rustc")?;
    let cargo = rustup_which("cargo").ok_or("rustup could not resolve cargo")?;
    let rust_bin = rustc
        .parent()
        .ok_or("rustc executable has no parent")?
        .to_path_buf();
    let rust_lib = rust_toolchain_root(&rustc)
        .ok_or("rust toolchain root not found")?
        .join("lib");
    let node = which::which("node")?;
    let node_root = node
        .parent()
        .ok_or("node executable has no parent")?
        .to_path_buf();
    let node_physical_root = resolve_reparse_target(&node_root)?;
    let node_modules = node_physical_root.join("node_modules");
    let npm_root = node_modules.join("npm");

    for path in [
        &python_root,
        &python_physical_root,
        &rust_bin,
        &rust_lib,
        &node_root,
        &node_physical_root,
        &node_modules,
        &npm_root,
    ] {
        if !path.exists() {
            return Err(format!("runtime path does not exist: {}", path.display()).into());
        }
    }

    Ok(RuntimeCapabilityRoots {
        python,
        python_root,
        python_physical_root,
        rustc,
        cargo,
        rust_bin,
        rust_lib,
        node_root,
        node_physical_root,
        node_modules,
        npm_root,
    })
}

#[cfg(windows)]
fn runtime_capability_grant_specs(roots: &RuntimeCapabilityRoots) -> Vec<(PathBuf, &'static str)> {
    vec![
        // Scoop's `current` directories are junctions. Grant traversal on the junction
        // and inheritable RX on the physical version root where DLL/script ACLs live.
        (roots.python_root.clone(), "RX"),
        (roots.python_physical_root.clone(), "(OI)(CI)RX"),
        (roots.rust_bin.clone(), "(OI)(CI)RX"),
        (roots.rust_lib.clone(), "(OI)(CI)RX"),
        (roots.node_root.clone(), "RX"),
        (roots.node_physical_root.clone(), "(OI)(CI)RX"),
        (roots.node_modules.clone(), "RX"),
        (roots.npm_root.clone(), "(OI)(CI)RX"),
    ]
}

#[cfg(windows)]
fn resolve_reparse_target(path: &Path) -> Result<PathBuf, Box<dyn std::error::Error>> {
    match fs::read_link(path) {
        Ok(target) if target.is_absolute() => Ok(target),
        Ok(target) => Ok(path
            .parent()
            .ok_or("reparse point has no parent")?
            .join(target)),
        Err(_) => Ok(path.to_path_buf()),
    }
}

#[cfg(windows)]
struct TemporaryCapabilityGrant {
    path: PathBuf,
    sid: String,
}

#[cfg(windows)]
impl TemporaryCapabilityGrant {
    fn apply(path: PathBuf, sid: &str, rights: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let started = Instant::now();
        grant_acl_shallow(&path, sid, rights)?;
        println!(
            "runtime_grant path={:?} rights={} elapsed_ms={}",
            path,
            rights,
            started.elapsed().as_millis()
        );
        Ok(Self {
            path,
            sid: sid.to_string(),
        })
    }
}

#[cfg(windows)]
impl Drop for TemporaryCapabilityGrant {
    fn drop(&mut self) {
        let started = Instant::now();
        match remove_acl_shallow(&self.path, &self.sid) {
            Ok(()) => println!(
                "runtime_remove path={:?} elapsed_ms={}",
                self.path,
                started.elapsed().as_millis()
            ),
            Err(error) => eprintln!("runtime_remove_failed path={:?} error={error}", self.path),
        }
    }
}

#[cfg(windows)]
fn cleanup_runtime_capability_grants() -> Result<(), Box<dyn std::error::Error>> {
    let roots = runtime_capability_roots()?;
    let capability = DerivedCapability::derive(RUNTIME_CAPABILITY_NAME)?;
    let sid = sid_string(capability.sid)?;
    println!("cleanup_runtime_capability_sid={sid}");
    for (path, _) in runtime_capability_grant_specs(&roots) {
        let started = Instant::now();
        let result = remove_acl_shallow(&path, &sid);
        println!(
            "cleanup_runtime_path path={:?} ok={} elapsed_ms={}",
            path,
            result.is_ok(),
            started.elapsed().as_millis()
        );
    }
    Ok(())
}

#[cfg(windows)]
fn run_runtime_capability_matrix() -> Result<(), Box<dyn std::error::Error>> {
    cleanup_runtime_capability_grants()?;

    let roots = runtime_capability_roots()?;
    let capability = DerivedCapability::derive(RUNTIME_CAPABILITY_NAME)?;
    let capability_sid = sid_string(capability.sid)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let moniker = format!(
        "CodingToolsMcp.Sandbox.RuntimeMatrix.{}.{}",
        std::process::id(),
        nonce
    );
    let root = std::env::temp_dir().join(format!("ctmcp-runtime-capability-{nonce}"));
    let inside = root.join("inside");
    fs::create_dir_all(&inside)?;
    fs::write(
        inside.join("Cargo.toml"),
        "[package]\nname='sandbox_runtime_fixture'\nversion='0.1.0'\nedition='2021'\n",
    )?;
    fs::create_dir_all(inside.join("src"))?;
    fs::write(inside.join("src/main.rs"), "fn main() {}\n")?;

    let profile = AppContainerProfile::create(&moniker, Some(capability.sid))?;
    let package_sid = sid_string(profile.sid)?;
    grant_acl(&root, &package_sid, "(OI)(CI)RX")?;
    grant_acl(&inside, &package_sid, "(OI)(CI)M")?;

    let mut grants = Vec::new();
    for (path, rights) in runtime_capability_grant_specs(&roots) {
        grants.push(TemporaryCapabilityGrant::apply(
            path,
            &capability_sid,
            rights,
        )?);
    }

    let python_args = vec![
        "-I".into(),
        "-S".into(),
        "-c".into(),
        "print('python-capability-ok')".into(),
    ];
    let control_python =
        run_external_capture(&roots.python, &python_args, &inside, profile.sid, None);

    let cargo_rustc_config = format!("build.rustc={:?}", roots.rustc.to_string_lossy());
    let cases = vec![
        ToolchainCase {
            name: "python-capability",
            executable: roots.python.clone(),
            args: python_args,
            grant_root: Some(roots.python_root.clone()),
        },
        ToolchainCase {
            name: "rustc-capability",
            executable: roots.rustc.clone(),
            args: vec!["--version".into()],
            grant_root: Some(roots.rust_bin.clone()),
        },
        ToolchainCase {
            name: "cargo-check-capability",
            executable: roots.cargo.clone(),
            args: vec![
                "check".into(),
                "--quiet".into(),
                "--config".into(),
                cargo_rustc_config,
            ],
            grant_root: Some(roots.rust_lib.clone()),
        },
        ToolchainCase {
            name: "npm-capability",
            executable: roots.node_physical_root.join("node.exe"),
            args: vec![
                "--preserve-symlinks".into(),
                "--preserve-symlinks-main".into(),
                roots
                    .npm_root
                    .join("bin")
                    .join("npm-cli.js")
                    .to_string_lossy()
                    .into_owned(),
                "--version".into(),
            ],
            grant_root: Some(roots.npm_root.clone()),
        },
    ];

    println!("runtime_matrix_moniker={moniker}");
    println!("runtime_matrix_package_sid={package_sid}");
    println!("runtime_matrix_capability_sid={capability_sid}");
    match &control_python {
        Ok(captured) => println!(
            "runtime_control_python_without_capability exit={} stdout={:?} stderr={:?}",
            captured.exit_code, captured.stdout, captured.stderr
        ),
        Err(error) => println!("runtime_control_python_without_capability launch_error={error}"),
    }

    let mut all_ok = control_python
        .as_ref()
        .map(|captured| captured.exit_code != 0)
        .unwrap_or(true);
    for case in cases {
        match run_external_capture(
            &case.executable,
            &case.args,
            &inside,
            profile.sid,
            Some(capability.sid),
        ) {
            Ok(captured) => {
                let ok = captured.exit_code == 0;
                all_ok &= ok;
                println!(
                    "runtime_matrix name={} ok={} exit={} exe={:?} grant_root={:?} stdout={:?} stderr={:?}",
                    case.name,
                    ok,
                    captured.exit_code,
                    case.executable,
                    case.grant_root,
                    captured.stdout,
                    captured.stderr
                );
            }
            Err(error) => {
                all_ok = false;
                println!(
                    "runtime_matrix name={} ok=false exe={:?} grant_root={:?} launch_error={error}",
                    case.name, case.executable, case.grant_root
                );
            }
        }
    }

    grants.clear();
    let residue_paths = [
        roots.python_physical_root.join("python.exe"),
        roots.rustc.clone(),
        roots.rust_lib.clone(),
        roots.node_physical_root.join("npm.cmd"),
        roots.npm_root.join("bin").join("npm-cli.js"),
    ];
    let mut residue = false;
    for path in residue_paths {
        let has_sid = acl_contains_sid(&path, &capability_sid)?;
        residue |= has_sid;
        println!("runtime_acl_residue path={:?} has_sid={has_sid}", path);
    }

    drop(profile);
    drop(capability);
    let _ = fs::remove_dir_all(&root);

    if residue {
        return Err("runtime capability SID remained in one or more ACLs after cleanup".into());
    }
    if !all_ok {
        return Err("one or more runtime capability cases failed".into());
    }
    Ok(())
}

#[cfg(windows)]
fn sandbox_state_environment(
    state: &Path,
) -> Result<Vec<(String, String)>, Box<dyn std::error::Error>> {
    let home = state.join("home");
    let temp = state.join("tmp");
    let cache = state.join("cache");
    let cargo_home = state.join("cargo-home");
    let cargo_target = state.join("cargo-target");
    let npm_cache = state.join("npm-cache");
    let npm_prefix = state.join("npm-prefix");
    let pycache = state.join("pycache");
    let appdata = home.join("AppData").join("Roaming");
    let local_appdata = home.join("AppData").join("Local");
    for path in [
        &home,
        &temp,
        &cache,
        &cargo_home,
        &cargo_target,
        &npm_cache,
        &npm_prefix,
        &pycache,
        &appdata,
        &local_appdata,
    ] {
        fs::create_dir_all(path)?;
    }

    let value = |path: &Path| path.to_string_lossy().into_owned();
    Ok(vec![
        ("TEMP".into(), value(&temp)),
        ("TMP".into(), value(&temp)),
        ("TMPDIR".into(), value(&temp)),
        ("HOME".into(), value(&home)),
        ("USERPROFILE".into(), value(&home)),
        ("APPDATA".into(), value(&appdata)),
        ("LOCALAPPDATA".into(), value(&local_appdata)),
        ("XDG_CACHE_HOME".into(), value(&cache)),
        ("CARGO_HOME".into(), value(&cargo_home)),
        ("CARGO_TARGET_DIR".into(), value(&cargo_target)),
        ("NPM_CONFIG_CACHE".into(), value(&npm_cache)),
        ("NPM_CONFIG_PREFIX".into(), value(&npm_prefix)),
        ("PYTHONPYCACHEPREFIX".into(), value(&pycache)),
    ])
}

#[cfg(windows)]
fn run_state_environment_matrix() -> Result<(), Box<dyn std::error::Error>> {
    cleanup_runtime_capability_grants()?;

    let roots = runtime_capability_roots()?;
    let capability = DerivedCapability::derive(RUNTIME_CAPABILITY_NAME)?;
    let capability_sid = sid_string(capability.sid)?;
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let moniker = format!(
        "CodingToolsMcp.Sandbox.StateEnv.{}.{}",
        std::process::id(),
        nonce
    );
    let root = std::env::temp_dir().join(format!("ctmcp-state-env-{nonce}"));
    let inside = root.join("inside");
    let state = root.join("state");
    fs::create_dir_all(inside.join("src"))?;
    fs::create_dir_all(&state)?;
    fs::write(
        inside.join("Cargo.toml"),
        "[package]\nname='sandbox_state_fixture'\nversion='0.1.0'\nedition='2021'\n",
    )?;
    fs::write(inside.join("src/main.rs"), "fn main() {}\n")?;
    let env_overrides = sandbox_state_environment(&state)?;

    let profile = AppContainerProfile::create(&moniker, Some(capability.sid))?;
    let package_sid = sid_string(profile.sid)?;
    grant_acl(&root, &package_sid, "(OI)(CI)RX")?;
    grant_acl(&inside, &package_sid, "(OI)(CI)M")?;
    grant_acl(&state, &package_sid, "(OI)(CI)M")?;

    let mut grants = Vec::new();
    for (path, rights) in runtime_capability_grant_specs(&roots) {
        grants.push(TemporaryCapabilityGrant::apply(
            path,
            &capability_sid,
            rights,
        )?);
    }

    let python_code = r#"import os, tempfile
f = tempfile.NamedTemporaryFile(delete=False)
f.write(b'sandbox-state-probe')
f.close()
print(f.name)
print(os.environ.get('USERPROFILE', ''))"#;
    let python = run_external_capture_with_env(
        &roots.python,
        &["-I".into(), "-S".into(), "-c".into(), python_code.into()],
        &inside,
        profile.sid,
        Some(capability.sid),
        &env_overrides,
    )?;
    let temp_dir = state.join("tmp");
    let home_dir = state.join("home");
    let python_stdout_lower = python.stdout.to_ascii_lowercase();
    let python_temp_ok = python.exit_code == 0
        && python_stdout_lower.contains(&temp_dir.to_string_lossy().to_ascii_lowercase())
        && python_stdout_lower.contains(&home_dir.to_string_lossy().to_ascii_lowercase());

    let cargo_rustc_config = format!("build.rustc={:?}", roots.rustc.to_string_lossy());
    let cargo = run_external_capture_with_env(
        &roots.cargo,
        &[
            "check".into(),
            "--quiet".into(),
            "--config".into(),
            cargo_rustc_config,
        ],
        &inside,
        profile.sid,
        Some(capability.sid),
        &env_overrides,
    )?;
    let cargo_target_ok = cargo.exit_code == 0 && state.join("cargo-target").exists();

    let npm_cli = roots.npm_root.join("bin").join("npm-cli.js");
    let npm = run_external_capture_with_env_timeout(
        &roots.node_physical_root.join("node.exe"),
        &[
            "--preserve-symlinks".into(),
            "--preserve-symlinks-main".into(),
            npm_cli.to_string_lossy().into_owned(),
            "cache".into(),
            "verify".into(),
            "--cache".into(),
            state.join("npm-cache").to_string_lossy().into_owned(),
        ],
        &inside,
        profile.sid,
        Some(capability.sid),
        &env_overrides,
        120_000,
    )?;
    let npm_cache_ok = npm.exit_code == 0 && state.join("npm-cache").exists();

    println!("state_env_moniker={moniker}");
    println!("state_env_package_sid={package_sid}");
    println!("state_env_capability_sid={capability_sid}");
    println!("state_env_root={:?}", state);
    println!(
        "state_env_python ok={} exit={} stdout={:?} stderr={:?}",
        python_temp_ok, python.exit_code, python.stdout, python.stderr
    );
    println!(
        "state_env_cargo ok={} exit={} target_exists={} stdout={:?} stderr={:?}",
        cargo_target_ok,
        cargo.exit_code,
        state.join("cargo-target").exists(),
        cargo.stdout,
        cargo.stderr
    );
    println!(
        "state_env_npm ok={} exit={} cache_exists={} stdout={:?} stderr={:?}",
        npm_cache_ok,
        npm.exit_code,
        state.join("npm-cache").exists(),
        npm.stdout,
        npm.stderr
    );

    let all_ok = python_temp_ok && cargo_target_ok && npm_cache_ok;
    grants.clear();
    let residue_paths = [
        roots.python_physical_root.join("python.exe"),
        roots.rustc.clone(),
        roots.rust_lib.clone(),
        roots.node_physical_root.join("node.exe"),
        npm_cli,
    ];
    let mut residue = false;
    for path in residue_paths {
        let has_sid = acl_contains_sid(&path, &capability_sid)?;
        residue |= has_sid;
        println!("state_env_acl_residue path={:?} has_sid={has_sid}", path);
    }

    drop(profile);
    drop(capability);
    let _ = fs::remove_dir_all(&root);

    if residue {
        return Err("state environment matrix left runtime capability ACL residue".into());
    }
    if !all_ok {
        return Err("one or more workspace-scoped state environment cases failed".into());
    }
    Ok(())
}

#[cfg(windows)]
fn run_toolchain_matrix() -> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let moniker = format!(
        "CodingToolsMcp.Sandbox.Matrix.{}.{}",
        std::process::id(),
        nonce
    );
    let root = std::env::temp_dir().join(format!("ctmcp-appcontainer-matrix-{nonce}"));
    let inside = root.join("inside");
    fs::create_dir_all(&inside)?;
    fs::write(
        inside.join("Cargo.toml"),
        "[package]\nname='sandbox_matrix_fixture'\nversion='0.1.0'\nedition='2021'\n",
    )?;
    fs::create_dir_all(inside.join("src"))?;
    fs::write(inside.join("src/main.rs"), "fn main() {}\n")?;

    let profile = AppContainerProfile::create(&moniker, None)?;
    let sid_string = sid_string(profile.sid)?;
    grant_acl(&root, &sid_string, "(OI)(CI)RX")?;
    grant_acl(&inside, &sid_string, "(OI)(CI)M")?;

    let mut cases = toolchain_cases()?;
    println!("moniker={moniker}");
    println!("sid={sid_string}");
    for case in cases.drain(..) {
        let mut result = run_toolchain_case(&case, &inside, profile.sid);
        result.grant_root = case.grant_root.clone();
        println!(
            "matrix name={} exe={:?} default_ok={} exit={:?} grant_root={:?} stdout={:?} stderr={:?} launch_error={:?}",
            result.name,
            result.executable,
            result.default_ok,
            result.final_exit_code,
            result.grant_root,
            result.stdout,
            result.stderr,
            result.launch_error,
        );
    }

    let _ = fs::remove_dir_all(&root);
    Ok(())
}

#[cfg(windows)]
#[derive(Clone)]
struct ToolchainCase {
    name: &'static str,
    executable: PathBuf,
    args: Vec<String>,
    grant_root: Option<PathBuf>,
}

#[cfg(windows)]
fn toolchain_cases() -> Result<Vec<ToolchainCase>, Box<dyn std::error::Error>> {
    let cmd = PathBuf::from(r"C:\Windows\System32\cmd.exe");
    let powershell = PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe");
    let git = which::which("git")?;
    let node = which::which("node")?;
    let python = preferred_python()?;
    let pwsh = which::which("pwsh")?;
    let cargo = rustup_which("cargo").unwrap_or(which::which("cargo")?);
    let rustc = rustup_which("rustc").unwrap_or(which::which("rustc")?);
    let npm = which::which("npm")?;
    let node_root = node.parent().map(Path::to_path_buf);

    Ok(vec![
        ToolchainCase {
            name: "cmd",
            executable: cmd,
            args: vec!["/d".into(), "/c".into(), "echo".into(), "cmd-ok".into()],
            grant_root: None,
        },
        ToolchainCase {
            name: "windows-powershell",
            executable: powershell,
            args: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "Write-Output powershell-ok".into(),
            ],
            grant_root: None,
        },
        ToolchainCase {
            name: "pwsh",
            executable: pwsh.clone(),
            args: vec![
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                "Write-Output pwsh-ok".into(),
            ],
            grant_root: pwsh.parent().map(Path::to_path_buf),
        },
        ToolchainCase {
            name: "git",
            executable: git.clone(),
            args: vec!["--version".into()],
            grant_root: git_install_root(&git),
        },
        ToolchainCase {
            name: "python",
            executable: python.clone(),
            args: vec!["-c".into(), "print('python-ok')".into()],
            grant_root: python.parent().map(Path::to_path_buf),
        },
        ToolchainCase {
            name: "node",
            executable: node.clone(),
            args: vec!["-e".into(), "console.log('node-ok')".into()],
            grant_root: node_root.clone(),
        },
        ToolchainCase {
            name: "npm",
            executable: PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            args: vec![
                "/d".into(),
                "/c".into(),
                npm.to_string_lossy().into_owned(),
                "--version".into(),
            ],
            grant_root: node_root,
        },
        ToolchainCase {
            name: "rustc",
            executable: rustc.clone(),
            args: vec!["--version".into()],
            grant_root: rust_toolchain_root(&rustc),
        },
        ToolchainCase {
            name: "cargo-check",
            executable: cargo.clone(),
            args: vec!["check".into(), "--quiet".into()],
            grant_root: rust_toolchain_root(&cargo),
        },
    ])
}

#[cfg(windows)]
fn run_toolchain_case(
    case: &ToolchainCase,
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
) -> ToolchainResult {
    match run_external_capture(&case.executable, &case.args, cwd, sid, None) {
        Ok(captured) => ToolchainResult {
            name: case.name.to_string(),
            executable: case.executable.clone(),
            default_ok: captured.exit_code == 0,
            grant_root: None,
            final_exit_code: Some(captured.exit_code),
            stdout: captured.stdout,
            stderr: captured.stderr,
            launch_error: None,
        },
        Err(error) => ToolchainResult {
            name: case.name.to_string(),
            executable: case.executable.clone(),
            default_ok: false,
            grant_root: None,
            final_exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            launch_error: Some(error.to_string()),
        },
    }
}

#[cfg(windows)]
fn run_external_capture(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
    capability_sid: Option<PSID>,
) -> Result<CapturedProcess, Box<dyn std::error::Error>> {
    run_external_capture_with_env(executable, args, cwd, sid, capability_sid, &[])
}

#[cfg(windows)]
fn run_external_capture_with_env(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
    capability_sid: Option<PSID>,
    env_overrides: &[(String, String)],
) -> Result<CapturedProcess, Box<dyn std::error::Error>> {
    run_external_capture_with_env_timeout(
        executable,
        args,
        cwd,
        sid,
        capability_sid,
        env_overrides,
        30_000,
    )
}

#[cfg(windows)]
fn run_external_capture_with_env_timeout(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
    capability_sid: Option<PSID>,
    env_overrides: &[(String, String)],
    wait_timeout_ms: u32,
) -> Result<CapturedProcess, Box<dyn std::error::Error>> {
    let pipes = StdioPipes::new()?;
    let job = JobGuard::new()?;
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let launched = launch_appcontainer(
        executable,
        &arg_refs,
        cwd,
        sid,
        capability_sid,
        env_overrides,
        Some((pipes.child_stdin, pipes.child_stdout, pipes.child_stderr)),
        true,
    )?;
    unsafe { AssignProcessToJobObject(job.handle, launched.process.hProcess)? };
    unsafe {
        if ResumeThread(launched.process.hThread) == u32::MAX {
            return Err("ResumeThread failed for toolchain case".into());
        }
    }
    pipes.close_child_ends();
    let stdin = unsafe { File::from_raw_handle(pipes.parent_stdin.0 as RawHandle) };
    let stdout = unsafe { File::from_raw_handle(pipes.parent_stdout.0 as RawHandle) };
    let stderr = unsafe { File::from_raw_handle(pipes.parent_stderr.0 as RawHandle) };
    drop(stdin);
    let stdout_thread = thread::spawn(move || read_file(stdout));
    let stderr_thread = thread::spawn(move || read_file(stderr));
    let wait = unsafe { WaitForSingleObject(launched.process.hProcess, wait_timeout_ms) };
    if wait != WAIT_OBJECT_0 {
        return Err(format!(
            "toolchain process did not exit within {wait_timeout_ms}ms: wait={wait:?}"
        )
        .into());
    }
    let exit_code = process_exit_code(launched.process.hProcess)?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| "stdout reader panicked")??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "stderr reader panicked")??;
    drop(launched);
    drop(job);
    Ok(CapturedProcess {
        exit_code,
        stdout,
        stderr,
    })
}

#[cfg(windows)]
struct CapturedProcess {
    exit_code: u32,
    stdout: String,
    stderr: String,
}

#[cfg(windows)]
fn preferred_python() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let candidates = which::which_all("python")?.collect::<Vec<_>>();
    candidates
        .iter()
        .find(|path| {
            path.to_string_lossy()
                .to_ascii_lowercase()
                .contains(r"\scoop\apps\python312\current\")
        })
        .cloned()
        .or_else(|| candidates.first().cloned())
        .ok_or_else(|| "python not found".into())
}

#[cfg(windows)]
fn rustup_which(tool: &str) -> Option<PathBuf> {
    let output = Command::new("rustup").args(["which", tool]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8(output.stdout).ok()?;
    let path = path.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

#[cfg(windows)]
fn rust_toolchain_root(executable: &Path) -> Option<PathBuf> {
    let mut current = executable.parent()?;
    while let Some(parent) = current.parent() {
        if current.file_name().and_then(|name| name.to_str()) == Some("bin") {
            return Some(parent.to_path_buf());
        }
        current = parent;
    }
    executable.parent().map(Path::to_path_buf)
}

#[cfg(windows)]
fn git_install_root(executable: &Path) -> Option<PathBuf> {
    let parent = executable.parent()?;
    if parent
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("cmd"))
    {
        parent.parent().map(Path::to_path_buf)
    } else {
        Some(parent.to_path_buf())
    }
}

#[cfg(windows)]
struct StdioResult {
    exit_code: u32,
    stdout: String,
    stderr: String,
    job_assigned: bool,
}

#[cfg(windows)]
fn run_stdio_roundtrip(
    executable: &Path,
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
) -> Result<StdioResult, Box<dyn std::error::Error>> {
    let pipes = StdioPipes::new()?;
    let job = JobGuard::new()?;
    let launched = launch_appcontainer(
        executable,
        &["--broker-child"],
        cwd,
        sid,
        None,
        &[],
        Some((pipes.child_stdin, pipes.child_stdout, pipes.child_stderr)),
        true,
    )?;

    unsafe { AssignProcessToJobObject(job.handle, launched.process.hProcess)? };
    let job_assigned = true;
    unsafe {
        if ResumeThread(launched.process.hThread) == u32::MAX {
            return Err("ResumeThread failed".into());
        }
    }

    pipes.close_child_ends();
    let mut stdin = unsafe { File::from_raw_handle(pipes.parent_stdin.0 as RawHandle) };
    let stdout = unsafe { File::from_raw_handle(pipes.parent_stdout.0 as RawHandle) };
    let stderr = unsafe { File::from_raw_handle(pipes.parent_stderr.0 as RawHandle) };

    let stdout_thread = thread::spawn(move || read_file(stdout));
    let stderr_thread = thread::spawn(move || read_file(stderr));
    stdin.write_all(b"broker-ping\n")?;
    drop(stdin);

    let wait = unsafe { WaitForSingleObject(launched.process.hProcess, 30_000) };
    if wait != WAIT_OBJECT_0 {
        return Err(format!("stdio child did not exit: wait={wait:?}").into());
    }
    let exit_code = process_exit_code(launched.process.hProcess)?;
    let stdout = stdout_thread
        .join()
        .map_err(|_| "stdout reader thread panicked")??;
    let stderr = stderr_thread
        .join()
        .map_err(|_| "stderr reader thread panicked")??;

    drop(launched);
    drop(job);
    Ok(StdioResult {
        exit_code,
        stdout,
        stderr,
        job_assigned,
    })
}

#[cfg(windows)]
fn run_job_kill_probe(
    executable: &Path,
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
) -> Result<bool, Box<dyn std::error::Error>> {
    let job = JobGuard::new()?;
    let launched = launch_appcontainer(
        executable,
        &["--sleep-child"],
        cwd,
        sid,
        None,
        &[],
        None,
        true,
    )?;
    unsafe { AssignProcessToJobObject(job.handle, launched.process.hProcess)? };
    unsafe {
        if ResumeThread(launched.process.hThread) == u32::MAX {
            return Err("ResumeThread failed for sleep child".into());
        }
    }
    thread::sleep(Duration::from_millis(150));
    let process_handle = launched.process.hProcess;
    drop(job);
    let wait = unsafe { WaitForSingleObject(process_handle, 5_000) };
    let terminated = wait == WAIT_OBJECT_0;
    drop(launched);
    Ok(terminated)
}

#[cfg(windows)]
fn read_file(mut file: File) -> Result<String, std::io::Error> {
    let mut output = Vec::new();
    file.read_to_end(&mut output)?;
    Ok(String::from_utf8_lossy(&output).into_owned())
}

#[cfg(windows)]
fn process_exit_code(handle: HANDLE) -> Result<u32, Box<dyn std::error::Error>> {
    let mut exit_code = u32::MAX;
    unsafe { GetExitCodeProcess(handle, &mut exit_code)? };
    Ok(exit_code)
}

#[cfg(windows)]
struct AppContainerProfile {
    moniker: HSTRING,
    sid: windows::Win32::Security::PSID,
}

#[cfg(windows)]
impl AppContainerProfile {
    fn create(
        moniker: &str,
        capability_sid: Option<PSID>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let moniker = HSTRING::from(moniker);
        let display = HSTRING::from("Coding Tools MCP AppContainer Broker Probe");
        let description = HSTRING::from("Disposable stdio and Job Object probe");
        let capability = capability_sid.map(|sid| SID_AND_ATTRIBUTES {
            Sid: sid,
            Attributes: SE_GROUP_ENABLED as u32,
        });
        let capabilities = capability.as_ref().map(std::slice::from_ref);
        let sid = unsafe {
            CreateAppContainerProfile(&moniker, &display, &description, capabilities)
                .or_else(|_| DeriveAppContainerSidFromAppContainerName(&moniker))?
        };
        Ok(Self { moniker, sid })
    }
}

#[cfg(windows)]
impl Drop for AppContainerProfile {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeSid(self.sid);
            let _ = DeleteAppContainerProfile(&self.moniker);
        }
    }
}

#[cfg(windows)]
struct DerivedCapability {
    sid: PSID,
    group_sids: *mut PSID,
    group_count: u32,
    capability_sids: *mut PSID,
    capability_count: u32,
}

#[cfg(windows)]
impl DerivedCapability {
    fn derive(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let name = HSTRING::from(name);
        let mut group_sids: *mut PSID = ptr::null_mut();
        let mut group_count = 0u32;
        let mut capability_sids: *mut PSID = ptr::null_mut();
        let mut capability_count = 0u32;
        unsafe {
            DeriveCapabilitySidsFromName(
                &name,
                &mut group_sids,
                &mut group_count,
                &mut capability_sids,
                &mut capability_count,
            )?;
        }
        if capability_count != 1 || capability_sids.is_null() {
            unsafe {
                free_sid_array(group_sids, group_count);
                free_sid_array(capability_sids, capability_count);
            }
            return Err(format!("expected one capability SID, got {capability_count}").into());
        }
        let sid = unsafe { *capability_sids };
        Ok(Self {
            sid,
            group_sids,
            group_count,
            capability_sids,
            capability_count,
        })
    }
}

#[cfg(windows)]
impl Drop for DerivedCapability {
    fn drop(&mut self) {
        unsafe {
            free_sid_array(self.group_sids, self.group_count);
            free_sid_array(self.capability_sids, self.capability_count);
        }
    }
}

#[cfg(windows)]
unsafe fn free_sid_array(array: *mut PSID, count: u32) {
    if array.is_null() {
        return;
    }
    for index in 0..count as usize {
        let sid = *array.add(index);
        if !sid.0.is_null() {
            let _ = LocalFree(Some(HLOCAL(sid.0.cast())));
        }
    }
    let _ = LocalFree(Some(HLOCAL(array.cast())));
}

#[cfg(windows)]
struct JobGuard {
    handle: HANDLE,
}

#[cfg(windows)]
impl JobGuard {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let handle = unsafe { CreateJobObjectW(None, None)? };
        let mut information = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        information.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &information as *const _ as *const c_void,
                mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }
        Ok(Self { handle })
    }
}

#[cfg(windows)]
impl Drop for JobGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(windows)]
struct StdioPipes {
    child_stdin: HANDLE,
    parent_stdin: HANDLE,
    parent_stdout: HANDLE,
    child_stdout: HANDLE,
    parent_stderr: HANDLE,
    child_stderr: HANDLE,
}

#[cfg(windows)]
impl StdioPipes {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: ptr::null_mut(),
            bInheritHandle: true.into(),
        };
        let mut child_stdin = HANDLE::default();
        let mut parent_stdin = HANDLE::default();
        let mut parent_stdout = HANDLE::default();
        let mut child_stdout = HANDLE::default();
        let mut parent_stderr = HANDLE::default();
        let mut child_stderr = HANDLE::default();
        unsafe {
            CreatePipe(
                &mut child_stdin,
                &mut parent_stdin,
                Some(&mut attributes),
                0,
            )?;
            CreatePipe(
                &mut parent_stdout,
                &mut child_stdout,
                Some(&mut attributes),
                0,
            )?;
            CreatePipe(
                &mut parent_stderr,
                &mut child_stderr,
                Some(&mut attributes),
                0,
            )?;
            SetHandleInformation(parent_stdin, HANDLE_FLAG_INHERIT.0, Default::default())?;
            SetHandleInformation(parent_stdout, HANDLE_FLAG_INHERIT.0, Default::default())?;
            SetHandleInformation(parent_stderr, HANDLE_FLAG_INHERIT.0, Default::default())?;
        }
        Ok(Self {
            child_stdin,
            parent_stdin,
            parent_stdout,
            child_stdout,
            parent_stderr,
            child_stderr,
        })
    }

    fn close_child_ends(&self) {
        unsafe {
            let _ = CloseHandle(self.child_stdin);
            let _ = CloseHandle(self.child_stdout);
            let _ = CloseHandle(self.child_stderr);
        }
    }
}

#[cfg(windows)]
struct LaunchedProcess {
    process: PROCESS_INFORMATION,
    attribute_list: LPPROC_THREAD_ATTRIBUTE_LIST,
    _attribute_bytes: Vec<u8>,
}

#[cfg(windows)]
impl Drop for LaunchedProcess {
    fn drop(&mut self) {
        unsafe {
            DeleteProcThreadAttributeList(self.attribute_list);
            let _ = CloseHandle(self.process.hThread);
            let _ = CloseHandle(self.process.hProcess);
        }
    }
}

#[cfg(windows)]
fn launch_appcontainer(
    executable: &Path,
    args: &[&str],
    cwd: &Path,
    sid: windows::Win32::Security::PSID,
    capability_sid: Option<PSID>,
    env_overrides: &[(String, String)],
    stdio: Option<(HANDLE, HANDLE, HANDLE)>,
    suspended: bool,
) -> Result<LaunchedProcess, Box<dyn std::error::Error>> {
    let mut attribute_size = 0usize;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut attribute_size) };
    if attribute_size == 0 {
        return Err("attribute-list size query returned zero".into());
    }
    let mut attribute_bytes = vec![0u8; attribute_size];
    let attribute_list = LPPROC_THREAD_ATTRIBUTE_LIST(attribute_bytes.as_mut_ptr().cast());
    unsafe {
        InitializeProcThreadAttributeList(Some(attribute_list), 1, None, &mut attribute_size)?
    };

    let mut capability = capability_sid.map(|capability_sid| SID_AND_ATTRIBUTES {
        Sid: capability_sid,
        Attributes: SE_GROUP_ENABLED as u32,
    });
    let capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: capability
            .as_mut()
            .map_or(ptr::null_mut(), |value| value as *mut SID_AND_ATTRIBUTES),
        CapabilityCount: u32::from(capability.is_some()),
        Reserved: 0,
    };
    unsafe {
        UpdateProcThreadAttribute(
            attribute_list,
            0,
            PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
            Some((&capabilities as *const SECURITY_CAPABILITIES).cast::<c_void>()),
            mem::size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )?;
    }

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attribute_list;
    let inherit_handles = if let Some((stdin, stdout, stderr)) = stdio {
        startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
        startup.StartupInfo.hStdInput = stdin;
        startup.StartupInfo.hStdOutput = stdout;
        startup.StartupInfo.hStdError = stderr;
        true
    } else {
        false
    };

    let command_line = std::iter::once(quote_windows_arg(executable))
        .chain(args.iter().map(|arg| quote_windows_text(arg)))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command_line_w = command_line
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let application = HSTRING::from(executable.to_string_lossy().as_ref());
    let current_dir = HSTRING::from(cwd.to_string_lossy().as_ref());
    let mut process = PROCESS_INFORMATION::default();
    let environment_block =
        (!env_overrides.is_empty()).then(|| build_environment_block(env_overrides));
    let flags = EXTENDED_STARTUPINFO_PRESENT
        | CREATE_NO_WINDOW
        | if environment_block.is_some() {
            CREATE_UNICODE_ENVIRONMENT
        } else {
            Default::default()
        }
        | if suspended {
            CREATE_SUSPENDED
        } else {
            Default::default()
        };

    let create_result = unsafe {
        CreateProcessW(
            &application,
            Some(PWSTR(command_line_w.as_mut_ptr())),
            None,
            None,
            inherit_handles,
            flags,
            environment_block
                .as_ref()
                .map(|block| block.as_ptr().cast::<c_void>()),
            &current_dir,
            &startup.StartupInfo,
            &mut process,
        )
    };
    if let Err(error) = create_result {
        unsafe { DeleteProcThreadAttributeList(attribute_list) };
        return Err(error.into());
    }
    Ok(LaunchedProcess {
        process,
        attribute_list,
        _attribute_bytes: attribute_bytes,
    })
}

#[cfg(windows)]
fn build_environment_block(overrides: &[(String, String)]) -> Vec<u16> {
    let mut values = BTreeMap::<String, (String, String)>::new();
    for (key, value) in std::env::vars() {
        values.insert(key.to_ascii_uppercase(), (key, value));
    }
    for (key, value) in overrides {
        values.insert(
            key.to_ascii_uppercase(),
            (key.to_string(), value.to_string()),
        );
    }

    let mut block = Vec::new();
    for (_, (key, value)) in values {
        block.extend(format!("{key}={value}").encode_utf16());
        block.push(0);
    }
    block.push(0);
    block
}

#[cfg(windows)]
fn is_current_process_appcontainer() -> Result<bool, Box<dyn std::error::Error>> {
    let mut token = HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let mut value = 0u32;
    let mut returned = 0u32;
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            Some((&mut value as *mut u32).cast::<c_void>()),
            mem::size_of::<u32>() as u32,
            &mut returned,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    result?;
    Ok(value != 0)
}

#[cfg(windows)]
fn sid_string(sid: windows::Win32::Security::PSID) -> Result<String, Box<dyn std::error::Error>> {
    let mut string_sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut string_sid)? };
    if string_sid.is_null() {
        return Err("ConvertSidToStringSidW returned null".into());
    }
    Ok(unsafe { PCWSTR(string_sid.0).to_string()? })
}

#[cfg(windows)]
fn grant_acl(path: &Path, sid: &str, rights: &str) -> Result<(), Box<dyn std::error::Error>> {
    let grant = format!("*{sid}:{rights}");
    let status = Command::new("icacls.exe")
        .arg(path)
        .args(["/grant", grant.as_str(), "/T", "/C", "/Q"])
        .status()?;
    if !status.success() {
        return Err(format!("icacls grant failed for {}: {status}", path.display()).into());
    }
    Ok(())
}

#[cfg(windows)]
fn grant_acl_shallow(
    path: &Path,
    sid: &str,
    rights: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let grant = format!("*{sid}:{rights}");
    let status = Command::new("icacls.exe")
        .arg(path)
        .args(["/grant", grant.as_str(), "/C", "/Q"])
        .status()?;
    if !status.success() {
        return Err(format!(
            "icacls shallow grant failed for {}: {status}",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(windows)]
fn remove_acl_shallow(path: &Path, sid: &str) -> Result<(), Box<dyn std::error::Error>> {
    let principal = format!("*{sid}");
    let status = Command::new("icacls.exe")
        .arg(path)
        .args(["/remove", principal.as_str(), "/C", "/Q"])
        .status()?;
    if !status.success() {
        return Err(format!(
            "icacls shallow remove failed for {}: {status}",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(windows)]
fn acl_contains_sid(path: &Path, sid: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let principal = format!("*{sid}");
    let output = Command::new("icacls.exe")
        .arg(path)
        .args(["/findsid", principal.as_str(), "/C", "/Q"])
        .output()?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(text.contains(sid))
}

#[cfg(windows)]
fn quote_windows_arg(path: &Path) -> String {
    quote_windows_text(path.to_string_lossy().as_ref())
}

#[cfg(windows)]
fn quote_windows_text(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}
