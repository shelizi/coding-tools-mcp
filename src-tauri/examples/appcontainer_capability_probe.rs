#[cfg(windows)]
use std::ffi::c_void;
#[cfg(windows)]
use std::fs;
#[cfg(windows)]
use std::mem;
#[cfg(windows)]
use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::process::Command;
#[cfg(windows)]
use std::ptr;
#[cfg(windows)]
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use windows::core::{HSTRING, PCWSTR, PWSTR};
#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, LocalFree, HLOCAL, WAIT_OBJECT_0};
#[cfg(windows)]
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
#[cfg(windows)]
use windows::Win32::Security::{
    DeriveCapabilitySidsFromName, FreeSid, PSID, SECURITY_CAPABILITIES, SID_AND_ATTRIBUTES,
};
#[cfg(windows)]
use windows::Win32::System::SystemServices::SE_GROUP_ENABLED;
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetExitCodeProcess,
    InitializeProcThreadAttributeList, UpdateProcThreadAttribute, WaitForSingleObject,
    CREATE_NO_WINDOW, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
};

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("appcontainer capability probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    println!("AppContainer capability probe is Windows-only");
}

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "--workspace-child") {
        return run_workspace_child(&args);
    }
    if args.get(1).is_some_and(|arg| arg == "--shared-helper") {
        println!("shared-helper-ok");
        return Ok(());
    }
    if args.get(1).is_some_and(|arg| arg == "--control-child") {
        return run_control_child(&args);
    }
    run_parent_probe()
}

#[cfg(windows)]
fn run_workspace_child(args: &[std::ffi::OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let own = PathBuf::from(args.get(2).ok_or("missing own folder")?);
    let other = PathBuf::from(args.get(3).ok_or("missing other folder")?);
    let helper = PathBuf::from(args.get(4).ok_or("missing helper")?);

    let own_write = fs::write(own.join("own-write.txt"), b"own\n").is_ok();
    let other_write = fs::write(other.join("cross-write.txt"), b"cross\n").is_ok();
    let helper_ok = Command::new(helper)
        .arg("--shared-helper")
        .status()
        .is_ok_and(|status| status.success());

    std::process::exit(match (own_write, other_write, helper_ok) {
        (true, false, true) => 0,
        (false, _, _) => 61,
        (_, true, _) => 62,
        (_, _, false) => 63,
    });
}

#[cfg(windows)]
fn run_control_child(args: &[std::ffi::OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let helper = PathBuf::from(args.get(2).ok_or("missing helper")?);
    let helper_succeeded = Command::new(helper)
        .arg("--shared-helper")
        .status()
        .is_ok_and(|status| status.success());
    std::process::exit(if helper_succeeded { 71 } else { 0 });
}

#[cfg(windows)]
fn run_parent_probe() -> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let root = std::env::temp_dir().join(format!("ctmcp-capability-probe-{nonce}"));
    let folder_a = root.join("folder-a");
    let folder_b = root.join("folder-b");
    let shared = root.join("shared-runtime");
    fs::create_dir_all(&folder_a)?;
    fs::create_dir_all(&folder_b)?;
    fs::create_dir_all(&shared)?;

    let current = std::env::current_exe()?;
    let child_a = folder_a.join("workspace-child.exe");
    let child_b = folder_b.join("workspace-child.exe");
    let helper = shared.join("shared-helper.exe");
    fs::copy(&current, &child_a)?;
    fs::copy(&current, &child_b)?;
    fs::copy(&current, &helper)?;

    let moniker_a = format!(
        "CodingToolsMcp.Sandbox.CapA.{}.{}",
        std::process::id(),
        nonce
    );
    let moniker_b = format!(
        "CodingToolsMcp.Sandbox.CapB.{}.{}",
        std::process::id(),
        nonce
    );
    let capability = DerivedCapability::derive("CodingToolsMcp.Sandbox.RuntimeProbe")?;
    let profile_a = AppContainerProfile::create(&moniker_a, capability.sid)?;
    let profile_b = AppContainerProfile::create(&moniker_b, capability.sid)?;

    let sid_a = sid_string(profile_a.sid)?;
    let sid_b = sid_string(profile_b.sid)?;
    let capability_sid = sid_string(capability.sid)?;

    // Preserve inherited user/system ACLs. AppContainer access is the intersection of
    // the ordinary user token and package/capability principals, so A still cannot use
    // B's folder unless A's package SID (or a shared capability) is explicitly granted.
    grant_acl(&root, &sid_a, "RX")?;
    grant_acl(&root, &sid_b, "RX")?;
    grant_acl(&folder_a, &sid_a, "(OI)(CI)M")?;
    grant_acl(&folder_b, &sid_b, "(OI)(CI)M")?;
    grant_acl(&shared, &capability_sid, "(OI)(CI)RX")?;

    let exit_a = launch_appcontainer(
        &child_a,
        &[
            "--workspace-child".to_string(),
            path_arg(&folder_a),
            path_arg(&folder_b),
            path_arg(&helper),
        ],
        &folder_a,
        profile_a.sid,
        Some(capability.sid),
    )?;
    let exit_b = launch_appcontainer(
        &child_b,
        &[
            "--workspace-child".to_string(),
            path_arg(&folder_b),
            path_arg(&folder_a),
            path_arg(&helper),
        ],
        &folder_b,
        profile_b.sid,
        Some(capability.sid),
    )?;
    let control_exit = launch_appcontainer(
        &child_a,
        &["--control-child".to_string(), path_arg(&helper)],
        &folder_a,
        profile_a.sid,
        None,
    )?;

    let a_own = folder_a.join("own-write.txt").is_file();
    let b_own = folder_b.join("own-write.txt").is_file();
    let cross_a_to_b = folder_b.join("cross-write.txt").exists();
    let cross_b_to_a = folder_a.join("cross-write.txt").exists();

    println!("package_a_sid={sid_a}");
    println!("package_b_sid={sid_b}");
    println!("shared_capability_sid={capability_sid}");
    println!("workspace_a_exit={exit_a}");
    println!("workspace_b_exit={exit_b}");
    println!("control_without_capability_exit={control_exit}");
    println!("workspace_a_own_write={a_own}");
    println!("workspace_b_own_write={b_own}");
    println!("workspace_a_cross_write={cross_a_to_b}");
    println!("workspace_b_cross_write={cross_b_to_a}");

    let passed = exit_a == 0
        && exit_b == 0
        && control_exit == 0
        && a_own
        && b_own
        && !cross_a_to_b
        && !cross_b_to_a;

    drop(capability);
    drop(profile_b);
    drop(profile_a);
    let _ = fs::remove_dir_all(&root);

    if !passed {
        return Err(format!(
            "capability contract failed: a={exit_a}, b={exit_b}, control={control_exit}, a_own={a_own}, b_own={b_own}, a_cross={cross_a_to_b}, b_cross={cross_b_to_a}"
        )
        .into());
    }
    Ok(())
}

#[cfg(windows)]
fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(windows)]
struct AppContainerProfile {
    moniker: HSTRING,
    sid: PSID,
}

#[cfg(windows)]
impl AppContainerProfile {
    fn create(moniker: &str, capability_sid: PSID) -> Result<Self, Box<dyn std::error::Error>> {
        let moniker = HSTRING::from(moniker);
        let display = HSTRING::from("Coding Tools MCP Capability Probe");
        let description = HSTRING::from("Disposable shared runtime capability probe");
        let capabilities = [SID_AND_ATTRIBUTES {
            Sid: capability_sid,
            Attributes: SE_GROUP_ENABLED as u32,
        }];
        let sid = unsafe {
            CreateAppContainerProfile(&moniker, &display, &description, Some(&capabilities))
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
fn launch_appcontainer(
    executable: &Path,
    args: &[String],
    cwd: &Path,
    appcontainer_sid: PSID,
    capability_sid: Option<PSID>,
) -> Result<u32, Box<dyn std::error::Error>> {
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

    let mut capability = capability_sid.map(|sid| SID_AND_ATTRIBUTES {
        Sid: sid,
        Attributes: SE_GROUP_ENABLED as u32,
    });
    let security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: appcontainer_sid,
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
            Some((&security_capabilities as *const SECURITY_CAPABILITIES).cast::<c_void>()),
            mem::size_of::<SECURITY_CAPABILITIES>(),
            None,
            None,
        )?;
    }

    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = attribute_list;

    let command_line = std::iter::once(quote_windows_arg(executable))
        .chain(args.iter().map(|value| quote_windows_text(value)))
        .collect::<Vec<_>>()
        .join(" ");
    let mut command_line_w = command_line
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let application = HSTRING::from(executable.to_string_lossy().as_ref());
    let current_dir = HSTRING::from(cwd.to_string_lossy().as_ref());
    let mut process = PROCESS_INFORMATION::default();
    let create_result = unsafe {
        CreateProcessW(
            &application,
            Some(PWSTR(command_line_w.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT | CREATE_NO_WINDOW,
            None,
            &current_dir,
            &startup.StartupInfo,
            &mut process,
        )
    };
    unsafe { DeleteProcThreadAttributeList(attribute_list) };
    create_result?;

    let wait = unsafe { WaitForSingleObject(process.hProcess, 30_000) };
    if wait != WAIT_OBJECT_0 {
        unsafe {
            let _ = CloseHandle(process.hThread);
            let _ = CloseHandle(process.hProcess);
        }
        return Err(format!("child did not exit: wait={wait:?}").into());
    }
    let mut exit_code = u32::MAX;
    unsafe {
        GetExitCodeProcess(process.hProcess, &mut exit_code)?;
        let _ = CloseHandle(process.hThread);
        let _ = CloseHandle(process.hProcess);
    }
    Ok(exit_code)
}

#[cfg(windows)]
fn sid_string(sid: PSID) -> Result<String, Box<dyn std::error::Error>> {
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
        .args(["/grant", grant.as_str(), "/C", "/Q"])
        .status()?;
    if !status.success() {
        return Err(format!("icacls grant failed for {}: {status}", path.display()).into());
    }
    Ok(())
}

#[cfg(windows)]
fn quote_windows_arg(path: &Path) -> String {
    quote_windows_text(path.to_string_lossy().as_ref())
}

#[cfg(windows)]
fn quote_windows_text(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}
