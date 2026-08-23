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
use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
#[cfg(windows)]
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
#[cfg(windows)]
use windows::Win32::Security::Isolation::{
    CreateAppContainerProfile, DeleteAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
};
#[cfg(windows)]
use windows::Win32::Security::{
    FreeSid, GetTokenInformation, TokenIsAppContainer, SECURITY_CAPABILITIES, TOKEN_QUERY,
};
#[cfg(windows)]
use windows::Win32::System::Threading::{
    CreateProcessW, DeleteProcThreadAttributeList, GetCurrentProcess, GetExitCodeProcess,
    InitializeProcThreadAttributeList, OpenProcessToken, UpdateProcThreadAttribute,
    WaitForSingleObject, EXTENDED_STARTUPINFO_PRESENT, LPPROC_THREAD_ATTRIBUTE_LIST,
    PROCESS_INFORMATION, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
};

#[cfg(windows)]
fn main() {
    if let Err(error) = run() {
        eprintln!("appcontainer isolation probe failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    println!("AppContainer isolation probe is Windows-only");
}

#[cfg(windows)]
fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args_os().collect::<Vec<_>>();
    if args.get(1).is_some_and(|arg| arg == "--sandbox-child") {
        return run_sandbox_child(&args);
    }
    if args.get(1).is_some_and(|arg| arg == "--sandbox-grandchild") {
        return run_sandbox_grandchild(&args);
    }

    run_parent_probe()
}

#[cfg(windows)]
fn run_sandbox_child(args: &[std::ffi::OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let inside = args
        .get(2)
        .ok_or("missing inside path")
        .map(PathBuf::from)?;
    let outside = args
        .get(3)
        .ok_or("missing outside path")
        .map(PathBuf::from)?;

    let token_is_appcontainer = is_current_process_appcontainer()?;
    let inside_result = fs::write(inside.join("inside-write.txt"), b"inside\n");
    let outside_result = fs::write(outside.join("outside-write.txt"), b"outside\n");
    let grandchild_status = Command::new(std::env::current_exe()?)
        .arg("--sandbox-grandchild")
        .arg(&inside)
        .arg(&outside)
        .status();
    let grandchild_ok = grandchild_status.is_ok_and(|status| status.success());

    let code = match (
        token_is_appcontainer,
        inside_result.is_ok(),
        outside_result.is_ok(),
        grandchild_ok,
    ) {
        (true, true, false, true) => 0,
        (_, true, true, _) => 42,
        (_, false, _, _) => 43,
        (_, _, _, false) => 44,
        (false, _, _, _) => 45,
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn run_sandbox_grandchild(args: &[std::ffi::OsString]) -> Result<(), Box<dyn std::error::Error>> {
    let inside = args
        .get(2)
        .ok_or("missing inside path")
        .map(PathBuf::from)?;
    let outside = args
        .get(3)
        .ok_or("missing outside path")
        .map(PathBuf::from)?;
    let token_is_appcontainer = is_current_process_appcontainer()?;
    let inside_result = fs::write(inside.join("grandchild-inside-write.txt"), b"inside\n");
    let outside_result = fs::write(outside.join("grandchild-outside-write.txt"), b"outside\n");
    let code = match (
        token_is_appcontainer,
        inside_result.is_ok(),
        outside_result.is_ok(),
    ) {
        (true, true, false) => 0,
        (_, true, true) => 52,
        (_, false, _) => 53,
        (false, _, _) => 54,
    };
    std::process::exit(code);
}

#[cfg(windows)]
fn run_parent_probe() -> Result<(), Box<dyn std::error::Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    let moniker = format!(
        "CodingToolsMcp.Sandbox.Probe.{}.{}",
        std::process::id(),
        nonce
    );
    let root = std::env::temp_dir().join(format!("ctmcp-appcontainer-probe-{nonce}"));
    let inside = root.join("inside");
    let outside = root.join("outside");
    fs::create_dir_all(&inside)?;
    fs::create_dir_all(&outside)?;

    let child_exe = inside.join("sandbox-child.exe");
    fs::copy(std::env::current_exe()?, &child_exe)?;

    let moniker_h = HSTRING::from(&moniker);
    let display_h = HSTRING::from("Coding Tools MCP AppContainer Probe");
    let description_h = HSTRING::from("Disposable filesystem isolation probe");

    let sid = unsafe {
        CreateAppContainerProfile(&moniker_h, &display_h, &description_h, None)
            .or_else(|_| DeriveAppContainerSidFromAppContainerName(&moniker_h))?
    };
    let profile_guard = AppContainerProfileGuard {
        moniker: moniker_h.clone(),
        sid,
    };
    let sid_string = sid_string(sid)?;

    grant_acl(&root, &sid_string, "(OI)(CI)RX")?;
    grant_acl(&inside, &sid_string, "(OI)(CI)M")?;

    let exit_code = launch_appcontainer_child(&child_exe, &inside, &outside, profile_guard.sid)?;

    let inside_created = inside.join("inside-write.txt").is_file();
    let outside_created = outside.join("outside-write.txt").exists();
    let grandchild_inside_created = inside.join("grandchild-inside-write.txt").is_file();
    let grandchild_outside_created = outside.join("grandchild-outside-write.txt").exists();
    println!("moniker={moniker}");
    println!("sid={sid_string}");
    println!("child_exit_code={exit_code}");
    println!("inside_write_created={inside_created}");
    println!("outside_write_created={outside_created}");
    println!("grandchild_inside_write_created={grandchild_inside_created}");
    println!("grandchild_outside_write_created={grandchild_outside_created}");

    let passed = exit_code == 0
        && inside_created
        && !outside_created
        && grandchild_inside_created
        && !grandchild_outside_created;
    drop(profile_guard);
    let _ = fs::remove_dir_all(&root);

    if !passed {
        return Err(format!(
            "isolation contract failed: exit={exit_code}, inside={inside_created}, outside={outside_created}, grandchild_inside={grandchild_inside_created}, grandchild_outside={grandchild_outside_created}"
        )
        .into());
    }
    Ok(())
}

#[cfg(windows)]
struct AppContainerProfileGuard {
    moniker: HSTRING,
    sid: windows::Win32::Security::PSID,
}

#[cfg(windows)]
impl Drop for AppContainerProfileGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = FreeSid(self.sid);
            let _ = DeleteAppContainerProfile(&self.moniker);
        }
    }
}

#[cfg(windows)]
fn is_current_process_appcontainer() -> Result<bool, Box<dyn std::error::Error>> {
    let mut token = windows::Win32::Foundation::HANDLE::default();
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)? };
    let mut is_appcontainer = 0u32;
    let mut returned = 0u32;
    let result = unsafe {
        GetTokenInformation(
            token,
            TokenIsAppContainer,
            Some((&mut is_appcontainer as *mut u32).cast::<c_void>()),
            mem::size_of::<u32>() as u32,
            &mut returned,
        )
    };
    unsafe {
        let _ = CloseHandle(token);
    }
    result?;
    Ok(is_appcontainer != 0)
}

#[cfg(windows)]
fn sid_string(sid: windows::Win32::Security::PSID) -> Result<String, Box<dyn std::error::Error>> {
    let mut string_sid = PWSTR::null();
    unsafe { ConvertSidToStringSidW(sid, &mut string_sid)? };
    if string_sid.is_null() {
        return Err("ConvertSidToStringSidW returned null".into());
    }
    // The probe process is short-lived. The tiny LocalAlloc buffer is intentionally
    // not separately freed here so the example does not need a System::Memory feature.
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
fn launch_appcontainer_child(
    executable: &Path,
    inside: &Path,
    outside: &Path,
    sid: windows::Win32::Security::PSID,
) -> Result<u32, Box<dyn std::error::Error>> {
    let mut attribute_bytes = Vec::<u8>::new();
    let mut attribute_size = 0usize;
    let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut attribute_size) };
    if attribute_size == 0 {
        return Err("InitializeProcThreadAttributeList did not report a buffer size".into());
    }
    attribute_bytes.resize(attribute_size, 0);
    let attribute_list = LPPROC_THREAD_ATTRIBUTE_LIST(attribute_bytes.as_mut_ptr().cast());
    unsafe {
        InitializeProcThreadAttributeList(Some(attribute_list), 1, None, &mut attribute_size)?
    };

    let security_capabilities = SECURITY_CAPABILITIES {
        AppContainerSid: sid,
        Capabilities: ptr::null_mut(),
        CapabilityCount: 0,
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

    let command_line = format!(
        "{} --sandbox-child {} {}",
        quote_windows_arg(executable),
        quote_windows_arg(inside),
        quote_windows_arg(outside)
    );
    let mut command_line_w = command_line
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let application_h = HSTRING::from(executable.to_string_lossy().as_ref());
    let current_dir_h = HSTRING::from(inside.to_string_lossy().as_ref());
    let mut process_info = PROCESS_INFORMATION::default();

    let create_result = unsafe {
        CreateProcessW(
            &application_h,
            Some(PWSTR(command_line_w.as_mut_ptr())),
            None,
            None,
            false,
            EXTENDED_STARTUPINFO_PRESENT,
            None,
            &current_dir_h,
            &startup.StartupInfo,
            &mut process_info,
        )
    };
    unsafe { DeleteProcThreadAttributeList(attribute_list) };
    create_result?;

    let wait = unsafe { WaitForSingleObject(process_info.hProcess, 30_000) };
    if wait != WAIT_OBJECT_0 {
        unsafe {
            let _ = CloseHandle(process_info.hThread);
            let _ = CloseHandle(process_info.hProcess);
        }
        return Err(format!("sandbox child did not exit normally: wait={wait:?}").into());
    }

    let mut exit_code = u32::MAX;
    unsafe {
        GetExitCodeProcess(process_info.hProcess, &mut exit_code)?;
        let _ = CloseHandle(process_info.hThread);
        let _ = CloseHandle(process_info.hProcess);
    }
    Ok(exit_code)
}

#[cfg(windows)]
fn quote_windows_arg(path: &Path) -> String {
    let text = path.to_string_lossy();
    format!("\"{}\"", text.replace('"', "\\\""))
}
