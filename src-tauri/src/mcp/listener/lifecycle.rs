use std::sync::Arc;

use axum::http::HeaderMap;
use tokio::sync::oneshot;

use crate::auth::{external_base_url, require_configured_secret, OAuthRuntime};
use crate::mcp::server::new_state;
use crate::secret::SecretStore;
use crate::tools::policy::PolicySettings;
use crate::tools::ExecutionLimits;
use crate::tunnel::append_profile_log_buffered;
use crate::workspace::{
    parse_bind_address, socket_addr_for_bind, url_host_for_bind, AuthConfig, RuntimeConfig,
    WorkspaceFolder,
};

use super::{routes::build_router, ListenerState};

pub type ShutdownSender = oneshot::Sender<()>;

#[allow(clippy::too_many_arguments)]
pub fn spawn_listener(
    port: u16,
    folders: Vec<WorkspaceFolder>,
    bootstrap_folder_id: String,
    workspace_id: String,
    auth: AuthConfig,
    public_base_url: String,
    oauth_client_secret: Option<String>,
    oauth_password: Option<String>,
    oauth_token_secret: Option<String>,
    runtime: RuntimeConfig,
) -> Result<(ShutdownSender, crate::task_runtime::JoinHandle<()>), String> {
    let policy = PolicySettings::from_runtime(&runtime);
    let redact_telemetry = policy.security_policy.redact_telemetry;
    let mcp = new_state(
        folders,
        bootstrap_folder_id,
        workspace_id.clone(),
        auth.clone(),
        policy,
        runtime.tool_profile.clone(),
        runtime.permission_mode.clone(),
        runtime.sandbox.clone(),
        ExecutionLimits::new_with_global(
            runtime.blocking_admission_limit as usize,
            runtime.process_admission_limit as usize,
            runtime.global_blocking_admission_limit as usize,
            runtime.global_process_admission_limit as usize,
            runtime.active_session_limit as usize,
        ),
    )?;
    let bearer_token = if auth.bearer_enabled() {
        let key = "bearer_token";
        let secret = if auth.use_shared_secrets {
            SecretStore::get_shared(key).map_err(|e| e.to_string())?
        } else {
            SecretStore::get(&workspace_id, key).map_err(|e| e.to_string())?
        };
        Some(require_configured_secret(secret, "MCP bearer token")?)
    } else {
        None
    };
    let oauth_client_secret = oauth_client_secret.filter(|value| !value.trim().is_empty());
    let configured_public_url = public_base_url.trim().to_string();
    let bind_address = parse_bind_address(&runtime.bind_address)?.to_string();
    let oauth = if auth.oauth_enabled() {
        let oauth_base = if configured_public_url.is_empty() {
            format!("http://{}:{port}", url_host_for_bind(&bind_address))
        } else {
            external_base_url(&HeaderMap::new(), port, &configured_public_url)
        };
        Some(Arc::new(OAuthRuntime::try_new(
            oauth_base,
            auth.oauth_client_id.clone(),
            oauth_client_secret.clone(),
            oauth_password,
            oauth_token_secret,
        )?))
    } else {
        None
    };
    let state = ListenerState {
        mcp,
        auth,
        workspace_id,
        bind_address: bind_address.clone(),
        bind_port: port,
        configured_public_url,
        bearer_token,
        oauth,
        oauth_client_secret,
        transport_mode: runtime.transport_mode.clone(),
        redact_telemetry,
    };
    // 在返回 Running 之前完成 bind，避免后台任务里的端口冲突被伪装成启动成功。
    let listener = bind_listener(&bind_address, port)?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let profile_id = state.workspace_id.clone();
    let handle = crate::task_runtime::spawn(async move {
        let result = serve(listener, state, shutdown_rx).await;
        if let Err(err) = &result {
            append_profile_log_buffered(
                &profile_id,
                "stderr.log",
                &format!("[mcp] listener stopped: {err}"),
            );
            eprintln!("mcp listener stopped: {err}");
        } else {
            append_profile_log_buffered(&profile_id, "stderr.log", "[mcp] listener stopped");
        }
    });
    Ok((shutdown_tx, handle))
}

async fn serve(
    listener: tokio::net::TcpListener,
    state: ListenerState,
    shutdown: oneshot::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let profile_id = state.workspace_id.clone();
    let transport_mode = state.transport_mode.clone();
    let listening_address = listener.local_addr()?;
    let app = build_router(state);

    append_profile_log_buffered(
        &profile_id,
        "stdout.log",
        &format!("[mcp] listening on http://{listening_address}/mcp transport={transport_mode}"),
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = shutdown.await;
        })
        .await?;
    Ok(())
}

pub(super) fn bind_listener(
    bind_address: &str,
    port: u16,
) -> Result<tokio::net::TcpListener, String> {
    let addr = socket_addr_for_bind(bind_address, port)?;
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|err| format!("MCP 監聽位址 {addr} 綁定失敗: {err}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("MCP 本地端口 {port} 设置非阻塞失败: {err}"))?;
    tokio::net::TcpListener::from_std(listener)
        .map_err(|err| format!("MCP 本地监听器初始化失败: {err}"))
}
