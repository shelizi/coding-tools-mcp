#![cfg_attr(target_os = "windows", allow(linker_messages))]

mod actions;
mod application;
mod app_state;
mod auth;
#[cfg(feature = "desktop")]
mod commands;
mod data;
mod error;
pub mod harness;
mod health;
mod mcp;
mod platform;
mod runtime;
mod secret;
mod settings;
mod task_runtime;
pub mod tools;
mod tunnel;
mod workspace;

/// Stable, UI-independent types for future headless and cross-platform hosts.
///
/// The desktop application remains the default feature. Consumers that only
/// need the core can build this library with `--no-default-features` without
/// pulling Tauri, GTK, or WebView dependencies into their binary.
pub mod core {
    pub use crate::application::runtime::{
        actions_runtime_status, mcp_runtime_status, restart_actions_runtime,
        restart_mcp_runtime, start_actions_runtime, start_mcp_runtime, stop_actions_runtime,
        stop_mcp_runtime,
    };
    pub use crate::app_state::{
        bootstrap_workspace, teardown_workspace, AppState,
    };
    pub use crate::data::{AppData, DataStore};
    pub use crate::error::{AppError, AppResult};
    pub use crate::runtime::{RuntimeSupervisor, ServiceKind};
    pub use crate::workspace::{
        ActionsConfig, AuthConfig, ExecutionTarget, McpPublicEndpoint, RuntimeConfig,
        RuntimeStatusDto, WorkspaceFolder, WorkspaceProfile, WslLocation,
    };
}

#[cfg(feature = "desktop")]
use app_state::AppState;
#[cfg(feature = "desktop")]
use commands::{
    add_workspace_folder, create_workspace, create_wsl_workspace, delete_frp_profile, delete_workspace,
    get_actions_runtime_status, get_app_settings, get_download_config, get_frp_snippet,
    get_last_workspace_id, get_proxy, get_runtime_status, get_shared_secret, get_workspace_secret,
    install_software, list_frp_profiles, list_history_sessions, list_software, list_workspaces,
    list_wsl_distributions,
    open_workspace_directory, read_history_session, read_workspace_logs, read_workspace_telemetry,
    regenerate_shared_secret, regenerate_workspace_secret, remove_workspace_folder,
    restart_actions_runtime, restart_runtime, restart_tunnel, run_health_checks, save_frp_profile,
    set_download_config, set_last_workspace, set_proxy, set_shared_secret, set_workspace_secret,
    start_actions_runtime, start_runtime, start_tunnel, stop_actions_runtime, stop_runtime,
    stop_tunnel, test_tunnel, uninstall_software, update_workspace,
};
#[cfg(feature = "desktop")]
use tauri::Manager;

#[cfg(feature = "desktop")]
fn show_main_window<R: tauri::Runtime>(app: &tauri::AppHandle<R>) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

#[cfg(all(feature = "desktop", target_os = "windows"))]
fn acquire_single_instance() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;

    // 保持 mutex HANDLE 到进程退出，由 Windows 自动回收。第二个实例必须在
    // cleanup_managed_frpc_instances 之前退出，否则会清理第一个实例的 frpc。
    let Ok(handle) = (unsafe {
        CreateMutexW(
            None,
            false,
            w!("Local\\CodingToolsMcpDesktop-SingleInstance"),
        )
    }) else {
        eprintln!("创建应用单实例锁失败，为避免误清理其他实例的 frpc，本次启动已取消");
        return false;
    };
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        let _ = unsafe { CloseHandle(handle) };
        return false;
    }
    true
}

#[cfg(all(feature = "desktop", not(target_os = "windows")))]
fn acquire_single_instance() -> bool {
    true
}

#[cfg(feature = "desktop")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    if !acquire_single_instance() {
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            use tauri::menu::MenuBuilder;
            use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

            let tray_menu = MenuBuilder::new(app)
                .text("show", "顯示主視窗")
                .text("quit", "結束程式")
                .build()?;
            let mut tray_builder = TrayIconBuilder::new()
                .menu(&tray_menu)
                .show_menu_on_left_click(false)
                .tooltip("Coding Tools MCP")
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray_builder = tray_builder.icon(icon.clone());
            }
            tray_builder.build(app)?;

            app.manage(AppState::new().expect("failed to load app state"));
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                if window.hide().is_ok() {
                    api.prevent_close();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_workspaces,
            list_history_sessions,
            read_history_session,
            read_workspace_telemetry,
            create_workspace,
            create_wsl_workspace,
            list_wsl_distributions,
            update_workspace,
            add_workspace_folder,
            remove_workspace_folder,
            open_workspace_directory,
            delete_workspace,
            start_runtime,
            stop_runtime,
            get_runtime_status,
            start_actions_runtime,
            stop_actions_runtime,
            get_actions_runtime_status,
            restart_runtime,
            restart_actions_runtime,
            get_frp_snippet,
            start_tunnel,
            stop_tunnel,
            run_health_checks,
            get_workspace_secret,
            set_workspace_secret,
            regenerate_workspace_secret,
            get_shared_secret,
            set_shared_secret,
            regenerate_shared_secret,
            read_workspace_logs,
            list_frp_profiles,
            save_frp_profile,
            delete_frp_profile,
            get_app_settings,
            restart_tunnel,
            test_tunnel,
            set_last_workspace,
            get_last_workspace_id,
            list_software,
            install_software,
            uninstall_software,
            get_download_config,
            set_download_config,
            get_proxy,
            set_proxy,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
