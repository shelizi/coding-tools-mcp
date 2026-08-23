// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(all(not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!(
    "release builds require `--features custom-protocol`; use `pnpm run desktop:portable`"
);

fn main() {
    if let Some(exit_code) =
        coding_tools_mcp_desktop_lib::run_appcontainer_acl_helper_if_requested()
    {
        std::process::exit(exit_code);
    }
    coding_tools_mcp_desktop_lib::run()
}
