// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(all(not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!(
    "release builds require `--features custom-protocol`; use `npm run desktop:portable`"
);

fn main() {
    coding_tools_mcp_desktop_lib::run()
}
