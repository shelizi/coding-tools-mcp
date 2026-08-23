#[cfg(windows)]
fn main() {
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    #[allow(unused_imports)]
    use windows::Win32::Security::Isolation::{
        CreateAppContainerProfile, DeriveAppContainerSidFromAppContainerName,
    };
    use windows::Win32::Security::SECURITY_CAPABILITIES;
    #[allow(unused_imports)]
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, InitializeProcThreadAttributeList,
        UpdateProcThreadAttribute, PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES, STARTUPINFOEXW,
    };

    let _ = ConvertSidToStringSidW;
    let _ = InitializeProcThreadAttributeList;
    let _ = UpdateProcThreadAttribute;
    let _ = DeleteProcThreadAttributeList;
    let _ = PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES;
    let _ = STARTUPINFOEXW::default();
    let _ = SECURITY_CAPABILITIES::default();
}

#[cfg(not(windows))]
fn main() {}
