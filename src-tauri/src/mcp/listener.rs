use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Form, Query, State};
use axum::http::{
    header::{ALLOW, CACHE_CONTROL, ORIGIN, WWW_AUTHENTICATE},
    HeaderMap, HeaderValue, StatusCode,
};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::oneshot;

use crate::auth::{
    authorization_server_metadata, authorize_get, authorize_post, external_base_url,
    protected_resource_metadata, protected_resource_metadata_url, require_configured_secret,
    token_exchange, verify_bearer_header, verify_oauth_bearer_header, AuthorizeForm,
    AuthorizeParams, OAuthRuntime, TokenForm,
};
use crate::mcp::server::{
    handle_request, handle_request_async, is_supported_protocol_version, new_state, SharedState,
    LATEST_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::mcp::session_activity;
use crate::mcp::telemetry::{
    begin_tool_request, format_request_log_value, record_tool_usage, ToolUsageInput,
};
use crate::secret::SecretStore;
use crate::tools::policy::PolicySettings;
use crate::tools::ExecutionLimits;
use crate::tunnel::append_profile_log_buffered;
use crate::workspace::{
    parse_bind_address, socket_addr_for_bind, url_host_for_bind, AuthConfig, RuntimeConfig,
    WorkspaceFolder,
};

pub type ShutdownSender = oneshot::Sender<()>;

#[derive(Clone)]
struct ListenerState {
    mcp: SharedState,
    auth: AuthConfig,
    workspace_id: String,
    bind_address: String,
    bind_port: u16,
    configured_public_url: String,
    bearer_token: Option<String>,
    oauth: Option<Arc<OAuthRuntime>>,
    oauth_client_secret: Option<String>,
    transport_mode: String,
}

fn unix_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn json_byte_len(value: &Value) -> usize {
    serde_json::to_vec(value)
        .map(|bytes| bytes.len())
        .unwrap_or(0)
}

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
    let mcp = new_state(
        folders,
        bootstrap_folder_id,
        workspace_id.clone(),
        auth.clone(),
        policy,
        runtime.tool_profile.clone(),
        runtime.permission_mode.clone(),
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

fn build_router(state: ListenerState) -> Router {
    let public_prefix = configured_route_prefix(&state.configured_public_url);
    let mut router = service_routes_for_prefix("")
        .route(
            "/.well-known/oauth-authorization-server",
            get(oauth_authorization_server_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(oauth_protected_resource_metadata),
        );
    if !public_prefix.is_empty() {
        let authorization_metadata = authorization_metadata_path(&public_prefix);
        let protected_metadata = protected_resource_metadata_path(&public_prefix);
        router = router
            .merge(service_routes_for_prefix(&public_prefix))
            .route(
                &authorization_metadata,
                get(oauth_authorization_server_metadata),
            )
            .route(&protected_metadata, get(oauth_protected_resource_metadata));
    }
    router.with_state(state)
}

fn service_routes_for_prefix(prefix: &str) -> Router<ListenerState> {
    let mcp = prefixed_route(prefix, "/mcp");
    let mcp_info_path = prefixed_route(prefix, "/mcp/info");
    let authorize = prefixed_route(prefix, "/oauth/authorize");
    let token = prefixed_route(prefix, "/oauth/token");

    Router::new()
        .route(&mcp, get(mcp_get).post(mcp_post).delete(mcp_delete))
        .route(&mcp_info_path, get(mcp_info))
        .route(
            &authorize,
            get(oauth_authorize_get).post(oauth_authorize_post),
        )
        .route(&token, post(oauth_token_post))
}

fn authorization_metadata_path(prefix: &str) -> String {
    format!(
        "/.well-known/oauth-authorization-server{}",
        prefix.trim_end_matches('/')
    )
}

fn protected_resource_metadata_path(prefix: &str) -> String {
    format!(
        "/.well-known/oauth-protected-resource{}/mcp",
        prefix.trim_end_matches('/')
    )
}

fn configured_route_prefix(configured_public_url: &str) -> String {
    reqwest::Url::parse(configured_public_url.trim())
        .ok()
        .map(|url| url.path().trim_end_matches('/').to_string())
        .filter(|path| !path.is_empty() && path != "/")
        .unwrap_or_default()
}

fn prefixed_route(prefix: &str, route: &str) -> String {
    if prefix.is_empty() {
        route.to_string()
    } else {
        format!("{}{}", prefix.trim_end_matches('/'), route)
    }
}

fn bind_listener(bind_address: &str, port: u16) -> Result<tokio::net::TcpListener, String> {
    let addr = socket_addr_for_bind(bind_address, port)?;
    let listener = std::net::TcpListener::bind(addr)
        .map_err(|err| format!("MCP 監聽位址 {addr} 綁定失敗: {err}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("MCP 本地端口 {port} 设置非阻塞失败: {err}"))?;
    tokio::net::TcpListener::from_std(listener)
        .map_err(|err| format!("MCP 本地监听器初始化失败: {err}"))
}

async fn mcp_info() -> Response {
    ([(CACHE_CONTROL, "no-store")], Json(mcp_discovery_payload())).into_response()
}

async fn mcp_get(State(state): State<ListenerState>, headers: HeaderMap) -> Response {
    if state.transport_mode == "legacy-json" {
        return mcp_info().await;
    }
    if let Some(response) = validate_standard_connection(&state, &headers, false) {
        return response;
    }
    method_not_allowed("POST")
}

async fn mcp_delete(State(state): State<ListenerState>, headers: HeaderMap) -> Response {
    if state.transport_mode == "legacy-json" {
        return method_not_allowed("GET, POST");
    }
    if let Some(response) = validate_standard_connection(&state, &headers, false) {
        return response;
    }
    method_not_allowed("POST")
}

fn mcp_discovery_payload() -> Value {
    json!({
        "name": "coding-tools-mcp",
        "version": env!("CARGO_PKG_VERSION"),
        "protocolVersion": LATEST_PROTOCOL_VERSION,
        "supportedProtocolVersions": SUPPORTED_PROTOCOL_VERSIONS,
        "transport": "streamable-http"
    })
}

fn resolve_oauth_base(state: &ListenerState, headers: &HeaderMap) -> String {
    external_base_url(headers, state.bind_port, &state.configured_public_url)
}

async fn mcp_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    let standard_transport = state.transport_mode != "legacy-json";
    if standard_transport {
        if let Some(response) = validate_standard_connection(&state, &headers, true) {
            return response;
        }
        if let Some(response) = validate_json_rpc_message(&body) {
            return response;
        }
    } else if let Some(response) = require_mcp_auth(&state, &headers) {
        return response;
    }

    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let request_id = body.get("id").cloned().unwrap_or(Value::Null);
    let tool_name = body
        .get("params")
        .and_then(|params| params.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let argument_value = body
        .get("params")
        .and_then(|params| params.get("arguments"))
        .cloned()
        .unwrap_or(Value::Null);
    let host_session_key = body
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get("openai/session"))
        .and_then(Value::as_str);
    let arguments = format_request_log_value(&argument_value);
    let is_notification = body.get("method").is_some() && body.get("id").is_none();
    let is_response = body.get("method").is_none()
        && body.get("id").is_some()
        && (body.get("result").is_some() || body.get("error").is_some());
    let started_ts_ms = unix_timestamp_ms();
    let started_at = Instant::now();
    let mut session_activity = session_activity::begin(
        &state.workspace_id,
        host_session_key,
        &tool_name,
        &argument_value,
        started_ts_ms,
    );
    let request_timing =
        (!tool_name.is_empty()).then(|| begin_tool_request(&state.workspace_id, started_ts_ms));
    let orchestration_gap_log = request_timing
        .as_ref()
        .and_then(|timing| timing.orchestration_gap_ms)
        .map(|gap| gap.to_string())
        .unwrap_or_else(|| "null".to_string());
    let burst_id_log = request_timing
        .as_ref()
        .map(|timing| timing.activity_burst_id.to_string())
        .unwrap_or_else(|| "null".to_string());
    let burst_sequence_log = request_timing
        .as_ref()
        .map(|timing| timing.activity_burst_sequence.to_string())
        .unwrap_or_else(|| "null".to_string());
    let concurrent_request = request_timing
        .as_ref()
        .is_some_and(|timing| timing.concurrent_request);
    let request_json_bytes = json_byte_len(&body);
    let protocol_version = headers
        .get("mcp-protocol-version")
        .and_then(|value| value.to_str().ok())
        .unwrap_or(LATEST_PROTOCOL_VERSION)
        .to_string();
    append_profile_log_buffered(
        &state.workspace_id,
        "mcp-requests.log",
        &format!(
            "[rpc] received ts={} id={} method={} tool={} orchestration_gap_ms={} burst_id={} burst_sequence={} concurrent_request={} arguments={}",
            started_ts_ms,
            request_id,
            method,
            tool_name,
            orchestration_gap_log,
            burst_id_log,
            burst_sequence_log,
            concurrent_request,
            arguments
        ),
    );

    if standard_transport && is_response {
        append_profile_log_buffered(
            &state.workspace_id,
            "mcp-requests.log",
            &format!(
                "[rpc] accepted_response id={} duration_ms={}",
                request_id,
                started_at.elapsed().as_millis()
            ),
        );
        return StatusCode::ACCEPTED.into_response();
    }

    let mcp = state.mcp.clone();
    let profile_id = state.workspace_id.clone();
    let fast_path = matches!(method.as_str(), "initialize" | "ping" | "tools/list")
        || method.starts_with("notifications/");
    let result: Result<Value, String> = if fast_path {
        Ok(handle_request(&mcp, &body))
    } else {
        Ok(handle_request_async(mcp, body).await)
    };
    match result {
        Ok(response) => {
            let duration_ms = started_at.elapsed().as_millis();
            append_profile_log_buffered(
                &profile_id,
                "mcp-requests.log",
                &format!(
                    "[rpc] completed id={} method={} tool={} duration_ms={}",
                    request_id, method, tool_name, duration_ms
                ),
            );
            if !tool_name.is_empty() {
                let result_is_error = response
                    .get("result")
                    .and_then(|result| result.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let structured_ok = response
                    .get("result")
                    .and_then(|result| result.get("structuredContent"))
                    .and_then(|structured| structured.get("ok"))
                    .and_then(Value::as_bool);
                let outcome = if response.get("error").is_some() {
                    "rpc_error"
                } else if result_is_error || structured_ok == Some(false) {
                    "tool_error"
                } else {
                    "success"
                };
                if let Some(activity) = session_activity.as_mut() {
                    activity.complete(outcome, started_ts_ms.saturating_add(duration_ms));
                }
                record_tool_usage(ToolUsageInput {
                    profile_id: &profile_id,
                    transport_mode: &state.transport_mode,
                    protocol_version: &protocol_version,
                    request_id: &request_id,
                    method: &method,
                    tool_name: &tool_name,
                    arguments: &argument_value,
                    request_json_bytes,
                    rpc_fast_path: fast_path,
                    request_timing: request_timing.as_ref().expect("tool request timing"),
                    started_ts_ms,
                    duration_ms,
                    outcome,
                    response: Some(&response),
                    worker_error: None,
                });
            }
            if tool_name == "exec_command" || tool_name == "exec_health_check" {
                let structured = response
                    .get("result")
                    .and_then(|result| result.get("structuredContent"));
                let status = structured
                    .and_then(|value| value.get("status"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let termination_reason = structured
                    .and_then(|value| value.get("termination_reason"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let exit_code = structured
                    .and_then(|value| value.get("exit_code"))
                    .map(Value::to_string)
                    .unwrap_or_default();
                let is_error = response
                    .get("result")
                    .and_then(|result| result.get("isError"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                append_profile_log_buffered(
                    &profile_id,
                    "mcp-requests.log",
                    &format!(
                        "[exec] id={} tool={} is_error={} status={} termination_reason={} exit_code={}",
                        request_id, tool_name, is_error, status, termination_reason, exit_code
                    ),
                );
            }
            if standard_transport && is_notification {
                StatusCode::ACCEPTED.into_response()
            } else {
                json_no_store(response)
            }
        }
        Err(error) => {
            let duration_ms = started_at.elapsed().as_millis();
            append_profile_log_buffered(
                &profile_id,
                "mcp-requests.log",
                &format!(
                    "[rpc] worker_failed id={} method={} tool={} error={error}",
                    request_id, method, tool_name
                ),
            );
            if !tool_name.is_empty() {
                let worker_error = error.to_string();
                if let Some(activity) = session_activity.as_mut() {
                    activity.complete("worker_failed", started_ts_ms.saturating_add(duration_ms));
                }
                record_tool_usage(ToolUsageInput {
                    profile_id: &profile_id,
                    transport_mode: &state.transport_mode,
                    protocol_version: &protocol_version,
                    request_id: &request_id,
                    method: &method,
                    tool_name: &tool_name,
                    arguments: &argument_value,
                    request_json_bytes,
                    rpc_fast_path: fast_path,
                    request_timing: request_timing.as_ref().expect("tool request timing"),
                    started_ts_ms,
                    duration_ms,
                    outcome: "worker_failed",
                    response: None,
                    worker_error: Some(&worker_error),
                });
            }
            let error_body = json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {
                    "code": -32603,
                    "message": "Exec RPC worker failed",
                    "data": {
                        "stage": "rpc_worker",
                        "reason": "worker_failed",
                        "retryable": true,
                        "suggestion": "重试请求或重启 MCP 运行时"
                    }
                }
            });
            if standard_transport && is_notification {
                transport_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    -32603,
                    "RPC worker failed",
                )
            } else {
                json_no_store(error_body)
            }
        }
    }
}

fn validate_standard_connection(
    state: &ListenerState,
    headers: &HeaderMap,
    _post_request: bool,
) -> Option<Response> {
    if !origin_is_allowed(state, headers) {
        return Some(transport_error(
            StatusCode::FORBIDDEN,
            -32000,
            "Invalid Origin header",
        ));
    }

    if let Some(version) = headers.get("mcp-protocol-version") {
        let Ok(version) = version.to_str() else {
            return Some(transport_error(
                StatusCode::BAD_REQUEST,
                -32600,
                "Invalid MCP-Protocol-Version header",
            ));
        };
        if !is_supported_protocol_version(version.trim()) {
            return Some(transport_error(
                StatusCode::BAD_REQUEST,
                -32600,
                &format!("Unsupported MCP protocol version: {}", version.trim()),
            ));
        }
    }

    require_mcp_auth(state, headers)
        .map(|response| with_authenticate_challenge(state, headers, response))
}

fn validate_json_rpc_message(body: &Value) -> Option<Response> {
    let Some(object) = body.as_object() else {
        return Some(transport_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "The request body must be one JSON-RPC message",
        ));
    };
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Some(transport_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "jsonrpc must be '2.0'",
        ));
    }

    let has_method = object.get("method").and_then(Value::as_str).is_some();
    let has_id = object.contains_key("id");
    let is_response =
        !has_method && has_id && (object.contains_key("result") || object.contains_key("error"));
    if !has_method && !is_response {
        return Some(transport_error(
            StatusCode::BAD_REQUEST,
            -32600,
            "Invalid JSON-RPC request, notification, or response",
        ));
    }
    None
}

fn origin_is_allowed(state: &ListenerState, headers: &HeaderMap) -> bool {
    let Some(origin) = headers.get(ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Some(origin) = normalized_origin(origin) else {
        return false;
    };

    if origin_matches_listener(state, &origin) {
        return true;
    }

    if matches!(
        origin.as_str(),
        "https://chatgpt.com" | "https://chat.openai.com"
    ) {
        return true;
    }

    normalized_origin(&state.configured_public_url).is_some_and(|allowed| allowed == origin)
}

fn origin_matches_listener(state: &ListenerState, origin: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(origin) else {
        return false;
    };
    if url.scheme() != "http" || url.port_or_known_default() != Some(state.bind_port) {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let Ok(bind_address) = parse_bind_address(&state.bind_address) else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return bind_address.is_loopback() || bind_address.is_unspecified();
    }
    let Ok(origin_address) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    bind_address.is_unspecified()
        || bind_address == origin_address
        || (bind_address.is_loopback() && origin_address.is_loopback())
}

fn normalized_origin(value: &str) -> Option<String> {
    let url = reqwest::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    Some(url.origin().ascii_serialization())
}

fn with_authenticate_challenge(
    state: &ListenerState,
    headers: &HeaderMap,
    mut response: Response,
) -> Response {
    if response.status() != StatusCode::UNAUTHORIZED {
        return response;
    }
    let challenge = if state.auth.oauth_enabled() {
        format!(
            "Bearer resource_metadata=\"{}\"",
            protected_resource_metadata_url(&resolve_oauth_base(state, headers))
        )
    } else {
        "Bearer".to_string()
    };
    if let Ok(value) = HeaderValue::from_str(&challenge) {
        response.headers_mut().insert(WWW_AUTHENTICATE, value);
    }
    response
}

fn transport_error(status: StatusCode, code: i64, message: &str) -> Response {
    (
        status,
        [(CACHE_CONTROL, "no-store")],
        Json(json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": {
                "code": code,
                "message": message
            }
        })),
    )
        .into_response()
}

fn json_no_store(value: Value) -> Response {
    ([(CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = StatusCode::METHOD_NOT_ALLOWED.into_response();
    response
        .headers_mut()
        .insert(ALLOW, HeaderValue::from_static(allow));
    response
}

fn require_mcp_auth(state: &ListenerState, headers: &HeaderMap) -> Option<Response> {
    if state.auth.bearer_enabled() {
        let expected = state.bearer_token.as_deref().unwrap_or("");
        return verify_bearer_header(headers, expected);
    }
    if state.auth.oauth_enabled() {
        if let Some(oauth) = state.oauth.as_ref() {
            let server_url = resolve_oauth_base(state, headers);
            return verify_oauth_bearer_header(headers, oauth, &server_url);
        }
    }
    None
}

async fn oauth_authorization_server_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
    if !state.auth.oauth_enabled() {
        return oauth_not_configured();
    }
    let base = resolve_oauth_base(&state, &headers);
    Json(authorization_server_metadata(
        &base,
        state.oauth_client_secret.as_deref(),
    ))
    .into_response()
}

async fn oauth_protected_resource_metadata(
    State(state): State<ListenerState>,
    headers: HeaderMap,
) -> Response {
    if !state.auth.oauth_enabled() {
        return oauth_not_configured();
    }
    Json(protected_resource_metadata(&resolve_oauth_base(
        &state, &headers,
    )))
    .into_response()
}

async fn oauth_authorize_get(
    State(state): State<ListenerState>,
    Query(params): Query<AuthorizeParams>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    authorize_get(oauth, params, None)
}

async fn oauth_authorize_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Form(form): Form<AuthorizeForm>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return oauth_not_configured();
    };
    authorize_post(oauth, form, &resolve_oauth_base(&state, &headers))
}

async fn oauth_token_post(
    State(state): State<ListenerState>,
    headers: HeaderMap,
    Form(form): Form<TokenForm>,
) -> Response {
    let Some(oauth) = state.oauth.as_ref() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "unsupported_grant_type" })),
        )
            .into_response();
    };
    token_exchange(oauth, &headers, form, &resolve_oauth_base(&state, &headers))
}

fn oauth_not_configured() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": "OAuth not configured" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::{header::CACHE_CONTROL, HeaderMap, StatusCode};
    use axum::response::IntoResponse;
    use axum::Json;
    use serde_json::json;
    use tempfile::TempDir;

    use crate::mcp::telemetry::format_log_value;
    use crate::tools::ToolContext;
    use crate::workspace::AuthConfig;

    use super::{
        authorization_metadata_path, bind_listener, build_router, configured_route_prefix,
        mcp_discovery_payload, mcp_get, mcp_info, mcp_post, origin_matches_listener,
        prefixed_route, protected_resource_metadata_path, ListenerState,
    };

    fn test_state(transport_mode: &str) -> (TempDir, TempDir, ListenerState) {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let harness = tempfile::tempdir().expect("harness tempdir");
        let mcp = Arc::new(
            ToolContext::for_test(workspace.path().to_path_buf(), harness.path().to_path_buf())
                .expect("tool context"),
        );
        let auth = AuthConfig {
            auth_type: "noauth".into(),
            ..AuthConfig::default()
        };
        let state = ListenerState {
            mcp,
            auth,
            workspace_id: "test-workspace".into(),
            bind_address: "127.0.0.1".into(),
            bind_port: 28_766,
            configured_public_url: "https://mcp.example.com".into(),
            bearer_token: None,
            oauth: None,
            oauth_client_secret: None,
            transport_mode: transport_mode.into(),
        };
        (workspace, harness, state)
    }

    #[test]
    fn bind_listener_reports_port_conflict_synchronously() {
        let occupied = std::net::TcpListener::bind(("0.0.0.0", 0)).expect("占用测试端口");
        let port = occupied.local_addr().expect("读取测试端口").port();

        assert!(bind_listener("0.0.0.0", port).is_err());
    }

    #[test]
    fn public_base_url_creates_path_scoped_routes() {
        assert_eq!(
            configured_route_prefix("https://mcp.example.com/clients/pc-a"),
            "/clients/pc-a"
        );
        assert_eq!(prefixed_route("/clients/pc-a", "/mcp"), "/clients/pc-a/mcp");
        assert_eq!(prefixed_route("", "/mcp"), "/mcp");
        assert_eq!(
            authorization_metadata_path("/clients/pc-a"),
            "/.well-known/oauth-authorization-server/clients/pc-a"
        );
        assert_eq!(
            protected_resource_metadata_path("/clients/pc-a"),
            "/.well-known/oauth-protected-resource/clients/pc-a/mcp"
        );
    }

    #[tokio::test]
    async fn path_scoped_mcp_endpoint_accepts_requests() {
        let (_workspace, _harness, mut state) = test_state("streamable-http");
        state.configured_public_url = "https://mcp.example.com/clients/pc-a".into();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(state)).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{address}/clients/pc-a/mcp"))
            .header("mcp-protocol-version", "2025-11-25")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }))
            .send()
            .await
            .expect("path-scoped MCP request");

        assert_eq!(response.status(), StatusCode::OK);
        server.abort();
    }

    #[tokio::test]
    async fn path_scoped_oauth_metadata_uses_standard_well_known_routes() {
        let (_workspace, _harness, mut state) = test_state("streamable-http");
        state.auth.auth_type = "oauth".into();
        state.configured_public_url = "https://mcp.example.com/clients/pc-a".into();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(state)).await;
        });
        let client = reqwest::Client::new();

        let authorization: serde_json::Value = client
            .get(format!(
                "http://{address}/.well-known/oauth-authorization-server/clients/pc-a"
            ))
            .send()
            .await
            .expect("authorization metadata request")
            .json()
            .await
            .expect("authorization metadata response");
        assert_eq!(
            authorization["issuer"],
            "https://mcp.example.com/clients/pc-a"
        );

        let protected: serde_json::Value = client
            .get(format!(
                "http://{address}/.well-known/oauth-protected-resource/clients/pc-a/mcp"
            ))
            .send()
            .await
            .expect("protected resource metadata request")
            .json()
            .await
            .expect("protected resource metadata response");
        assert_eq!(
            protected["resource"],
            "https://mcp.example.com/clients/pc-a/mcp"
        );
        assert_eq!(
            protected["authorization_servers"],
            json!(["https://mcp.example.com/clients/pc-a"])
        );
        server.abort();
    }

    #[test]
    fn configured_listener_origin_is_allowed() {
        let (_workspace, _harness, mut state) = test_state("streamable-http");
        state.bind_address = "192.168.1.20".into();

        assert!(origin_matches_listener(&state, "http://192.168.1.20:28766"));
        assert!(!origin_matches_listener(
            &state,
            "http://192.168.1.21:28766"
        ));
    }

    #[test]
    fn wildcard_listener_accepts_numeric_interface_origins() {
        let (_workspace, _harness, mut state) = test_state("streamable-http");
        state.bind_address = "0.0.0.0".into();

        assert!(origin_matches_listener(&state, "http://192.168.1.20:28766"));
        assert!(!origin_matches_listener(&state, "http://example.com:28766"));
    }

    #[tokio::test]
    async fn discovery_reports_the_current_package_version() {
        let discovery = mcp_discovery_payload();

        assert_eq!(discovery["version"], env!("CARGO_PKG_VERSION"));
    }

    #[tokio::test]
    async fn discovery_prevents_stale_tool_catalog_caching() {
        let response = mcp_info().await.into_response();

        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn standard_get_returns_method_not_allowed() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let response = mcp_get(State(state), HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn legacy_get_keeps_discovery_fallback() {
        let (_workspace, _harness, state) = test_state("legacy-json");
        let response = mcp_get(State(state), HeaderMap::new()).await;

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn standard_notification_returns_accepted_without_rpc_body() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let response = mcp_post(
            State(state),
            HeaderMap::new(),
            Json(json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized"
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    #[serial_test::serial(process_runtime)]
    async fn real_http_disconnect_cancels_exec_and_releases_session_capacity() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let context = state.mcp.clone();
        let active_session_limit = context.sessions.active_session_limit();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(state)).await;
        });

        #[cfg(windows)]
        let command = "powershell -NoProfile -Command \"Start-Sleep -Seconds 30\"";
        #[cfg(unix)]
        let command = "sh -c \"sleep 30\"";
        let request = tokio::spawn(async move {
            reqwest::Client::new()
                .post(format!("http://{address}/mcp"))
                .header("mcp-protocol-version", "2025-11-25")
                .json(&json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "tools/call",
                    "params": {
                        "name": "exec_command",
                        "arguments": {
                            "cmd": command,
                            "timeout_ms": 60_000,
                            "yield_time_ms": 30_000,
                            "output_mode": "none"
                        }
                    }
                }))
                .send()
                .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while context.sessions.active_slots_available()
                == context.sessions.active_session_limit()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("HTTP request did not start an exec session");
        assert_eq!(
            context.sessions.active_slots_available(),
            active_session_limit - 1
        );

        request.abort();
        let _ = request.await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            context.sessions.active_slots_available(),
            active_session_limit - 1,
            "HTTP disconnect should preserve the session during the reconnect grace period"
        );
        tokio::time::timeout(Duration::from_secs(10), async {
            while context.sessions.active_slots_available()
                != context.sessions.active_session_limit()
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("HTTP disconnect did not release exec session capacity");
        server.abort();
    }

    #[tokio::test]
    async fn raw_http_can_discover_and_call_new_tools_without_client_schema_cache() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test listener");
        let address = listener.local_addr().expect("test listener address");
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, build_router(state)).await;
        });
        let client = reqwest::Client::new();
        let endpoint = format!("http://{address}/mcp");

        let listed: serde_json::Value = client
            .post(&endpoint)
            .header("mcp-protocol-version", "2025-11-25")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }))
            .send()
            .await
            .expect("tools/list request")
            .json()
            .await
            .expect("tools/list response");
        let tools = listed["result"]["tools"].as_array().expect("tool list");
        let names = tools
            .iter()
            .filter_map(|tool| tool["name"].as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"exec_many"));
        assert!(names.contains(&"wait_command"));
        assert!(names.contains(&"query_tool_usage"));
        assert!(!names.contains(&"write_stdin"));
        let search = tools
            .iter()
            .find(|tool| tool["name"] == "search_text")
            .expect("search_text schema");
        assert_eq!(
            search["inputSchema"]["properties"]["context_lines"]["maximum"],
            1000
        );

        let executed: serde_json::Value = client
            .post(&endpoint)
            .header("mcp-protocol-version", "2025-11-25")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": {
                    "name": "exec_many",
                    "arguments": {
                        "commands": [{"program": "cargo", "args": ["--version"]}]
                    }
                }
            }))
            .send()
            .await
            .expect("exec_many request")
            .json()
            .await
            .expect("exec_many response");
        assert_eq!(
            executed["result"]["structuredContent"]["command_ok"], true,
            "{executed}"
        );
        assert_eq!(
            executed["result"]["structuredContent"]["commands_executed"], 1,
            "{executed}"
        );
        server.abort();
    }

    #[tokio::test]
    async fn standard_initialize_negotiates_chatgpt_compatible_version() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let response = mcp_post(
            State(state),
            HeaderMap::new(),
            Json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": {"name": "test", "version": "1"}
                }
            })),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[CACHE_CONTROL], "no-store");
    }

    #[tokio::test]
    async fn standard_transport_rejects_bad_version_and_origin() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let mut headers = HeaderMap::new();
        headers.insert("mcp-protocol-version", "unsupported".parse().unwrap());
        let response = mcp_get(State(state.clone()), headers).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://attacker.example".parse().unwrap());
        let response = mcp_get(State(state), headers).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn standard_transport_accepts_chatgpt_origin() {
        let (_workspace, _harness, state) = test_state("streamable-http");
        let mut headers = HeaderMap::new();
        headers.insert("origin", "https://chatgpt.com".parse().unwrap());

        let response = mcp_get(State(state), headers).await;

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[test]
    fn request_log_keeps_command_and_redacts_sensitive_fields() {
        let value = json!({
            "cmd": "git status",
            "reason": "inspect workspace",
            "api_key": "should-not-be-logged",
            "nested": {
                "access-token": "also-secret"
            }
        });

        let logged = format_log_value(&value);

        assert!(logged.contains("git status"));
        assert!(logged.contains("inspect workspace"));
        assert!(!logged.contains("should-not-be-logged"));
        assert!(!logged.contains("also-secret"));
        assert_eq!(logged.matches("[REDACTED]").count(), 2);
    }

    #[test]
    fn request_log_truncates_multibyte_text_safely() {
        let value = json!({
            "items": vec!["指令內容"; 5_000]
        });

        let logged = format_log_value(&value);

        assert!(logged.ends_with("...[TRUNCATED]"));
        assert!(logged.is_char_boundary(logged.len()));
    }

    #[test]
    fn request_log_redacts_secrets_embedded_in_command_text() {
        let value = json!({
            "cmd": "curl --token super-secret -H 'Authorization: Bearer abc.def.ghi' API_KEY=hidden"
        });

        let logged = format_log_value(&value);

        assert!(logged.contains("--token [REDACTED]"));
        assert!(logged.contains("Authorization: [REDACTED]"));
        assert!(logged.contains("API_KEY=[REDACTED]"));
        assert!(!logged.contains("super-secret"));
        assert!(!logged.contains("abc.def.ghi"));
        assert!(!logged.contains("hidden"));
    }
}
