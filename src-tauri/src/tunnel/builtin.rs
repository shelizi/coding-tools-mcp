mod connection;
mod endpoint;
mod identity;
mod metrics;
mod pool_policy;
mod protocol_io;
mod request_mapping;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use coding_tools_tunnel_protocol::{
    decode_multiplex_binary_frame, encode_multiplex_binary_frame, ControlMessage, HeaderPair,
    MultiplexBinaryFrameKind, MultiplexControlMessage, MultiplexDemand, TunnelService,
    WorkerDemand, WorkerPolicy, MAX_REQUEST_BODY_BYTES,
};
use ed25519_dalek::{SigningKey, VerifyingKey};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, watch, Mutex, RwLock};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{interval, sleep, timeout, Instant, Interval, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::workspace::WorkspaceProfile;

use super::TunnelServiceKind;
use connection::{
    connect_authenticated_tunnel, connect_split_control_or_fallback, connect_split_data,
    reconnect_split_control, AuthenticatedMultiplexConnection, AuthenticatedSplitControlConnection,
    AuthenticatedTunnelConnection, AuthenticatedWorkerConnection, SplitControlNegotiation,
};
use endpoint::{builtin_endpoint_for_client, parse_builtin_endpoint};
use identity::{decode_server_verifying_key, decode_signing_key, load_or_enroll_device_identity};
pub use metrics::BuiltinTunnelSnapshot;
use metrics::{BuiltinTunnelMetrics, ConnectedWorkerGuard};
use pool_policy::{
    configured_burst_warm_floor, configured_max_connecting, jittered_limit, join_worker_indices,
    multiplex_pool_adjustment, next_reconnect_base, pool_adjustment, reconnect_delay,
    scale_down_reason, scale_up_block, scale_up_reason, worker_should_recycle,
    MultiplexPoolSnapshot, PoolCounts, INITIAL_RECONNECT_DELAY,
};
use protocol_io::{
    close_client_websocket, decode_control, decode_multiplex_control, send_control, send_heartbeat,
    ClientSink, ClientStream, HeartbeatTracker, HEARTBEAT_TIMEOUT,
};
use request_mapping::{prepare_local_request, response_headers, IncomingRequest};

const INITIAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WEBSOCKET_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const LOCAL_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);
const DEMAND_HINT_TTL: Duration = Duration::from_secs(3);

pub(crate) fn behavioral_parity_fixture() -> serde_json::Value {
    serde_json::json!({
        "local_connect_timeout_ms": LOCAL_CONNECT_TIMEOUT.as_millis(),
        "demand_hint_ttl_ms": DEMAND_HINT_TTL.as_millis(),
        "heartbeat_interval_ms": HEARTBEAT_INTERVAL.as_millis(),
        "heartbeat_timeout_ms": HEARTBEAT_TIMEOUT.as_millis(),
        "reconnect_initial_ms": INITIAL_RECONNECT_DELAY.as_millis(),
        "reconnect_max_ms": pool_policy::MAX_RECONNECT_DELAY.as_millis()
    })
}

#[derive(Clone)]
pub struct BuiltinTunnelConfig {
    pub public_url: String,
    pub websocket_url: String,
    pub client_id: String,
    pub service: TunnelService,
    pub route_prefix: String,
    pub local_base_url: String,
    device_id: String,
    signing_key: Arc<SigningKey>,
    server_id: String,
    server_verifying_key: Arc<VerifyingKey>,
    log_path: PathBuf,
}

struct BuiltinTunnelBaseConfig {
    public_url: String,
    client_id: String,
    service: TunnelService,
    local_base_url: String,
    log_path: PathBuf,
}

pub struct BuiltinTunnelHandle {
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
    metrics: Arc<BuiltinTunnelMetrics>,
    route_commands: mpsc::Sender<SplitRouteCommand>,
}

type SplitRouteTable = Arc<RwLock<HashMap<u32, BuiltinTunnelConfig>>>;

enum SplitRouteCommand {
    Add {
        config: BuiltinTunnelConfig,
        response: oneshot::Sender<Result<(u32, u16), String>>,
    },
    Remove {
        client_id: String,
        service: TunnelService,
        response: oneshot::Sender<Result<usize, String>>,
    },
}

impl BuiltinTunnelHandle {
    pub fn is_running(&self) -> bool {
        !self.task.is_finished()
    }

    pub fn snapshot(&self) -> BuiltinTunnelSnapshot {
        self.metrics.snapshot()
    }

    pub fn supports_shared_routes(&self) -> bool {
        !self.route_commands.is_closed()
    }

    pub async fn add_route(
        &self,
        profile: &WorkspaceProfile,
        kind: TunnelServiceKind,
    ) -> AppResult<String> {
        let (config, public_url) = builtin_tunnel_config(profile, kind).await?;
        loop {
            let (response, result) = oneshot::channel();
            self.route_commands
                .send(SplitRouteCommand::Add {
                    config: config.clone(),
                    response,
                })
                .await
                .map_err(|_| {
                    AppError::Message("目前內建隧道連線不支援共享 Workspace route。".into())
                })?;
            match result.await {
                Ok(result) => {
                    result.map_err(AppError::Message)?;
                    return Ok(public_url);
                }
                Err(_) if !self.route_commands.is_closed() => continue,
                Err(_) => return Err(AppError::Message("共享 Workspace route 註冊中斷。".into())),
            }
        }
    }

    pub async fn remove_route(&self, client_id: &str, kind: TunnelServiceKind) -> AppResult<usize> {
        let service = tunnel_service(kind);
        loop {
            let (response, result) = oneshot::channel();
            self.route_commands
                .send(SplitRouteCommand::Remove {
                    client_id: client_id.to_string(),
                    service,
                    response,
                })
                .await
                .map_err(|_| {
                    AppError::Message("目前內建隧道連線不支援共享 Workspace route。".into())
                })?;
            match result.await {
                Ok(result) => return result.map_err(AppError::Message),
                Err(_) if !self.route_commands.is_closed() => continue,
                Err(_) => return Err(AppError::Message("共享 Workspace route 移除中斷。".into())),
            }
        }
    }

    pub async fn stop(self) {
        let _ = self.shutdown.send(true);
        let mut task = self.task;
        if timeout(Duration::from_secs(5), &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
        }
    }
}

fn tunnel_service(kind: TunnelServiceKind) -> TunnelService {
    match kind {
        TunnelServiceKind::Mcp => TunnelService::Mcp,
        TunnelServiceKind::Actions => TunnelService::Actions,
    }
}

pub fn builtin_hub_key(profile: &WorkspaceProfile, kind: TunnelServiceKind) -> AppResult<String> {
    let (public_url, service) = match kind {
        TunnelServiceKind::Mcp => (profile.tunnel.public_url.as_str(), TunnelService::Mcp),
        TunnelServiceKind::Actions => (profile.actions.public_url.as_str(), TunnelService::Actions),
    };
    Ok(parse_builtin_endpoint(public_url, service)?.websocket_url)
}

pub fn builtin_route_client_id(public_url: &str, kind: TunnelServiceKind) -> AppResult<String> {
    Ok(parse_builtin_endpoint(public_url, tunnel_service(kind))?.client_id)
}

pub fn validate_builtin_tunnel(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<()> {
    let (public_url, service) = match kind {
        TunnelServiceKind::Mcp => (profile.tunnel.public_url.as_str(), TunnelService::Mcp),
        TunnelServiceKind::Actions => (profile.actions.public_url.as_str(), TunnelService::Actions),
    };
    parse_builtin_endpoint(public_url, service)?;
    Ok(())
}

fn builtin_base_config(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<BuiltinTunnelBaseConfig> {
    let (public_url, local_port, bind_address, service) = match kind {
        TunnelServiceKind::Mcp => (
            profile.tunnel.public_url.as_str(),
            profile.runtime.local_port,
            profile.runtime.bind_address.as_str(),
            TunnelService::Mcp,
        ),
        TunnelServiceKind::Actions => (
            profile.actions.public_url.as_str(),
            profile.actions.local_port,
            profile.actions.bind_address.as_str(),
            TunnelService::Actions,
        ),
    };
    let endpoint = parse_builtin_endpoint(public_url, service)?;
    Ok(BuiltinTunnelBaseConfig {
        public_url: endpoint.public_url,
        client_id: endpoint.client_id,
        service,
        local_base_url: format!("http://{}:{local_port}", local_connect_host(bind_address)),
        log_path: crate::platform::platform()
            .app_config_dir()?
            .join("logs")
            .join(&profile.id)
            .join(match kind {
                TunnelServiceKind::Mcp => "builtin-tunnel.log",
                TunnelServiceKind::Actions => "actions-builtin-tunnel.log",
            }),
    })
}

async fn builtin_tunnel_config(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<(BuiltinTunnelConfig, String)> {
    let base = builtin_base_config(profile, kind)?;
    let identity =
        load_or_enroll_device_identity(&profile.id, &base.public_url, &base.client_id).await?;
    let endpoint =
        builtin_endpoint_for_client(&base.public_url, base.service, &identity.client_id)?;
    let signing_key = decode_signing_key(&identity.private_key)?;
    let server_verifying_key = decode_server_verifying_key(&identity.server_public_key)?;
    let config = BuiltinTunnelConfig {
        public_url: endpoint.public_url,
        websocket_url: endpoint.websocket_url,
        client_id: endpoint.client_id,
        service: base.service,
        route_prefix: endpoint.route_prefix,
        local_base_url: base.local_base_url,
        device_id: identity.device_id,
        signing_key: Arc::new(signing_key),
        server_id: identity.server_id,
        server_verifying_key: Arc::new(server_verifying_key),
        log_path: base.log_path,
    };
    let public_url = config.public_url.clone();
    Ok((config, public_url))
}

pub async fn spawn_builtin_tunnel(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
) -> AppResult<(BuiltinTunnelHandle, String)> {
    let (config, public_url) = builtin_tunnel_config(profile, kind).await?;
    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (status_tx, mut status_rx) = mpsc::channel::<Result<(), String>>(32);
    let (route_commands, route_commands_rx) = mpsc::channel(32);
    let mut task = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
        route_commands_rx,
    ));

    let deadline = Instant::now() + INITIAL_CONNECT_TIMEOUT;
    let mut last_error = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            stop_startup_task(&shutdown, &mut task).await;
            return Err(AppError::Message(format!(
                "內建 WSS 隧道連線逾時{}",
                last_error
                    .map(|error| format!("：{error}"))
                    .unwrap_or_default()
            )));
        }
        match timeout(remaining, status_rx.recv()).await {
            Ok(Some(Ok(()))) => {
                return Ok((
                    BuiltinTunnelHandle {
                        shutdown,
                        task,
                        metrics,
                        route_commands,
                    },
                    public_url,
                ));
            }
            Ok(Some(Err(error))) => last_error = Some(error),
            Ok(None) => {
                stop_startup_task(&shutdown, &mut task).await;
                return Err(AppError::Message(
                    "內建 WSS 隧道 worker 在連線前停止。".into(),
                ));
            }
            Err(_) => continue,
        }
    }
}

async fn stop_startup_task(shutdown: &watch::Sender<bool>, task: &mut JoinHandle<()>) {
    let _ = shutdown.send(true);
    if timeout(Duration::from_secs(2), &mut *task).await.is_err() {
        task.abort();
        let _ = task.await;
    }
}

async fn run_worker_pool(
    config: BuiltinTunnelConfig,
    mut shutdown: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    mut route_commands: mpsc::Receiver<SplitRouteCommand>,
) {
    let mut attempt = 0_u64;
    let mut desired_routes = vec![config.clone()];
    loop {
        if *shutdown.borrow() || desired_routes.is_empty() {
            return;
        }
        let bootstrap_config = desired_routes[0].clone();
        metrics.set_peer_identity(&bootstrap_config.server_id, &bootstrap_config.device_id);
        let connection_id = format!("{}-control-{}", bootstrap_config.device_id, Uuid::new_v4());
        match connect_split_control_or_fallback(&bootstrap_config, &connection_id).await {
            Ok(SplitControlNegotiation::Split(control)) => {
                metrics.set_split_session(
                    &control.session_id,
                    control.route_id,
                    control.stream_limit,
                    &control.initial_policy,
                );
                attempt = 0;
                match run_split_session_pool(
                    bootstrap_config,
                    control,
                    shutdown.clone(),
                    status_tx.clone(),
                    metrics.clone(),
                    &mut route_commands,
                    &mut desired_routes,
                )
                .await
                {
                    Ok(()) => return,
                    Err(error) => {
                        append_log(&config.log_path, &format!("split control session: {error}"));
                        metrics.set_last_error(Some(error.clone()));
                        let _ = status_tx.send(Err(error)).await;
                    }
                }
            }
            Ok(SplitControlNegotiation::Fallback(connection)) => {
                metrics.set_legacy_mode();
                drop(route_commands);
                run_legacy_worker_pool(
                    bootstrap_config,
                    shutdown,
                    status_tx,
                    metrics,
                    Some(connection),
                )
                .await;
                return;
            }
            Err(error) => {
                metrics.set_last_error(Some(error.clone()));
                let _ = status_tx.send(Err(error)).await;
            }
        }
        attempt = attempt.saturating_add(1);
        let delay = reconnect_delay(INITIAL_RECONNECT_DELAY, 0, attempt);
        tokio::select! {
            _ = shutdown.changed() => return,
            _ = sleep(delay) => {}
        }
    }
}

async fn reconnect_split_control_during_grace(
    config: &BuiltinTunnelConfig,
    session_id: &str,
    route_id: u32,
    stream_limit: u16,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<Option<AuthenticatedSplitControlConnection>, String> {
    let delays = [
        Duration::ZERO,
        Duration::from_millis(100),
        Duration::from_millis(250),
    ];
    let mut last_error = "split control reconnect failed".to_string();
    for delay in delays {
        if *shutdown.borrow() {
            return Ok(None);
        }
        if !delay.is_zero() {
            tokio::select! {
                _ = shutdown.changed() => return Ok(None),
                _ = sleep(delay) => {}
            }
        }
        let connection_id = format!("{}-control-{}", config.device_id, Uuid::new_v4());
        let attempt = timeout(
            Duration::from_millis(500),
            reconnect_split_control(config, &connection_id, session_id, route_id, stream_limit),
        );
        let result = tokio::select! {
            _ = shutdown.changed() => return Ok(None),
            result = attempt => result,
        };
        match result {
            Ok(Ok(control)) => return Ok(Some(control)),
            Ok(Err(error)) => last_error = error,
            Err(_) => last_error = "split control reconnect attempt timed out".into(),
        }
    }
    Err(last_error)
}

async fn run_split_session_pool(
    config: BuiltinTunnelConfig,
    control: AuthenticatedSplitControlConnection,
    mut shutdown: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    route_commands: &mut mpsc::Receiver<SplitRouteCommand>,
    desired_routes: &mut Vec<BuiltinTunnelConfig>,
) -> Result<(), String> {
    let AuthenticatedSplitControlConnection {
        mut sink,
        mut stream,
        initial_policy,
        session_id,
        route_id,
        stream_limit,
        connection_id,
    } = control;
    metrics.set_multiplex_policy(&initial_policy);
    let routes: SplitRouteTable =
        Arc::new(RwLock::new(HashMap::from([(route_id, config.clone())])));
    for route_config in desired_routes.iter() {
        if route_config.client_id == config.client_id && route_config.service == config.service {
            continue;
        }
        let registration_id = Uuid::new_v4().to_string();
        protocol_io::send_multiplex_control(
            &mut sink,
            &MultiplexControlMessage::RouteRegister {
                registration_id: registration_id.clone(),
                client_id: route_config.client_id.clone(),
                service: route_config.service,
            },
        )
        .await?;
        let (replayed_route_id, replayed_stream_limit) = loop {
            match protocol_io::receive_multiplex_control(&mut sink, &mut stream).await? {
                MultiplexControlMessage::RouteRegistered {
                    registration_id: accepted,
                    route_id,
                    stream_limit,
                } if accepted == registration_id => break (route_id, stream_limit),
                MultiplexControlMessage::CapacityUpdate { .. } => {}
                MultiplexControlMessage::Error { message, .. } => return Err(message),
                _ => return Err("server did not replay shared split route".into()),
            }
        };
        if replayed_route_id == 0
            || replayed_stream_limit == 0
            || replayed_stream_limit > initial_policy.streams_per_connection
        {
            return Err("server returned invalid replayed shared route capacity".into());
        }
        routes
            .write()
            .await
            .insert(replayed_route_id, route_config.clone());
    }
    append_log(
        &config.log_path,
        &format!(
            "split control authenticated: connection_id={connection_id} session_id={session_id} route_id={route_id} stream_limit={stream_limit}"
        ),
    );
    let mut workers = JoinSet::<(usize, bool)>::new();
    let mut managed = HashMap::<usize, ManagedWorker>::new();
    let (event_tx, mut event_rx) = mpsc::channel(128);
    let mut next_worker_index = 0_usize;
    let mut latest_demand: Option<MultiplexDemand> = None;
    let mut demand_seen_at: Option<Instant> = None;
    let mut idle_excess_since: Option<Instant> = None;
    let mut heartbeat = HeartbeatTracker::new_at(Instant::now());
    let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
    heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut reconcile_tick = interval(Duration::from_millis(250));
    reconcile_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    for _ in 0..usize::from(initial_policy.start_connections) {
        spawn_split_data_worker(
            &mut workers,
            &mut managed,
            &mut next_worker_index,
            &config,
            &shutdown,
            &status_tx,
            &metrics,
            &event_tx,
            &session_id,
            routes.clone(),
            stream_limit,
        );
    }

    let result = loop {
        tokio::select! {
            _ = shutdown.changed() => break Ok(()),
            _ = heartbeat_interval.tick() => {
                if let Err(error) = send_heartbeat(&mut sink, &heartbeat).await {
                    append_log(&config.log_path, &format!("split control heartbeat failed; rebinding session: {error}"));
                    let Some(control) = reconnect_split_control_during_grace(
                        &config, &session_id, route_id, stream_limit, &mut shutdown,
                    ).await? else { break Ok(()); };
                    sink = control.sink;
                    stream = control.stream;
                    metrics.set_multiplex_policy(&control.initial_policy);
                    heartbeat = HeartbeatTracker::new_at(Instant::now());
                    continue;
                }
            }
            incoming = stream.next() => {
                heartbeat.record_activity();
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<MultiplexControlMessage>(text.as_str())
                            .map_err(|error| error.to_string())? {
                            MultiplexControlMessage::CapacityUpdate { demand } => {
                                latest_demand = Some(demand);
                                demand_seen_at = Some(Instant::now());
                            }
                            MultiplexControlMessage::Error { message, .. } => break Err(message),
                            _ => break Err("unexpected split control message".into()),
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        sink.send(Message::Pong(payload)).await.map_err(|error| error.to_string())?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        let Some(control) = reconnect_split_control_during_grace(
                            &config, &session_id, route_id, stream_limit, &mut shutdown,
                        ).await? else { break Ok(()); };
                        sink = control.sink;
                        stream = control.stream;
                        metrics.set_multiplex_policy(&control.initial_policy);
                        heartbeat = HeartbeatTracker::new_at(Instant::now());
                        continue;
                    }
                    Some(Ok(Message::Binary(_))) => break Err("binary frame received on split control connection".into()),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        append_log(&config.log_path, &format!("split control read failed; rebinding session: {error}"));
                        let Some(control) = reconnect_split_control_during_grace(
                            &config, &session_id, route_id, stream_limit, &mut shutdown,
                        ).await? else { break Ok(()); };
                        sink = control.sink;
                        stream = control.stream;
                        metrics.set_multiplex_policy(&control.initial_policy);
                        heartbeat = HeartbeatTracker::new_at(Instant::now());
                        continue;
                    }
                }
            }
            command = route_commands.recv() => {
                match command {
                    Some(SplitRouteCommand::Add { config: route_config, response }) => {
                        if route_config.websocket_url != config.websocket_url
                            || route_config.device_id != config.device_id
                            || route_config.server_id != config.server_id
                        {
                            let _ = response.send(Err("shared route does not belong to this device/server session".into()));
                            continue;
                        }
                        if let Some((existing_route, _)) = routes.read().await.iter().find(|(_, existing)| existing.client_id == route_config.client_id && existing.service == route_config.service) {
                            let _ = response.send(Ok((*existing_route, initial_policy.streams_per_connection)));
                            continue;
                        }
                        if !desired_routes.iter().any(|existing| existing.client_id == route_config.client_id && existing.service == route_config.service) {
                            desired_routes.push(route_config.clone());
                        }
                        let registered = 'register: loop {
                            let registration_id = Uuid::new_v4().to_string();
                            if let Err(error) = protocol_io::send_multiplex_control(&mut sink, &MultiplexControlMessage::RouteRegister {
                                registration_id: registration_id.clone(),
                                client_id: route_config.client_id.clone(),
                                service: route_config.service,
                            }).await {
                                append_log(&config.log_path, &format!("split route register send failed; rebinding session: {error}"));
                                let Some(control) = reconnect_split_control_during_grace(
                                    &config, &session_id, route_id, stream_limit, &mut shutdown,
                                ).await? else { return Ok(()); };
                                sink = control.sink;
                                stream = control.stream;
                                metrics.set_multiplex_policy(&control.initial_policy);
                                heartbeat = HeartbeatTracker::new_at(Instant::now());
                                continue 'register;
                            }
                            loop {
                                match protocol_io::receive_multiplex_control(&mut sink, &mut stream).await {
                                    Ok(MultiplexControlMessage::RouteRegistered { registration_id: accepted, route_id, stream_limit }) if accepted == registration_id => break 'register Ok((route_id, stream_limit)),
                                    Ok(MultiplexControlMessage::CapacityUpdate { demand }) => { latest_demand = Some(demand); demand_seen_at = Some(Instant::now()); }
                                    Ok(MultiplexControlMessage::Error { message, .. }) => break 'register Err(message),
                                    Ok(_) => break 'register Err("server did not register shared split route".into()),
                                    Err(error) => {
                                        append_log(&config.log_path, &format!("split route register response failed; rebinding session: {error}"));
                                        let Some(control) = reconnect_split_control_during_grace(
                                            &config, &session_id, route_id, stream_limit, &mut shutdown,
                                        ).await? else { return Ok(()); };
                                        sink = control.sink;
                                        stream = control.stream;
                                        metrics.set_multiplex_policy(&control.initial_policy);
                                        heartbeat = HeartbeatTracker::new_at(Instant::now());
                                        continue 'register;
                                    }
                                }
                            }
                        };
                        match registered {
                            Ok((new_route_id, new_stream_limit)) if new_route_id != 0 && new_stream_limit > 0 && new_stream_limit <= initial_policy.streams_per_connection => {
                                routes.write().await.insert(new_route_id, route_config.clone());
                                let _ = response.send(Ok((new_route_id, new_stream_limit)));
                            }
                            Ok(_) => {
                                desired_routes.retain(|existing| !(existing.client_id == route_config.client_id && existing.service == route_config.service));
                                let _ = response.send(Err("server returned invalid shared route capacity".into()));
                            }
                            Err(error) => {
                                desired_routes.retain(|existing| !(existing.client_id == route_config.client_id && existing.service == route_config.service));
                                let _ = response.send(Err(error));
                            }
                        }
                    }
                    Some(SplitRouteCommand::Remove { client_id, service, response }) => {
                        let route_id_to_remove = {
                            let routes = routes.read().await;
                            routes.iter().find_map(|(route_id, config)| (config.client_id == client_id && config.service == service).then_some(*route_id))
                        };
                        if let Some(route_id_to_remove) = route_id_to_remove {
                            desired_routes.retain(|config| !(config.client_id == client_id && config.service == service));
                            loop {
                                match protocol_io::send_multiplex_control(&mut sink, &MultiplexControlMessage::RouteUnregister { route_id: route_id_to_remove }).await {
                                    Ok(()) => break,
                                    Err(error) => {
                                        append_log(&config.log_path, &format!("split route unregister send failed; rebinding session: {error}"));
                                        let Some(control) = reconnect_split_control_during_grace(
                                            &config, &session_id, route_id, stream_limit, &mut shutdown,
                                        ).await? else { return Ok(()); };
                                        sink = control.sink;
                                        stream = control.stream;
                                        metrics.set_multiplex_policy(&control.initial_policy);
                                        heartbeat = HeartbeatTracker::new_at(Instant::now());
                                    }
                                }
                            }
                            routes.write().await.remove(&route_id_to_remove);
                        } else {
                            desired_routes.retain(|config| !(config.client_id == client_id && config.service == service));
                        }
                        let remaining = routes.read().await.len();
                        let _ = response.send(Ok(remaining));
                    }
                    None => {}
                }
            }
            event = event_rx.recv() => {
                if let Some(WorkerEvent::State { worker_index, state }) = event {
                    if let Some(worker) = managed.get_mut(&worker_index) {
                        if state == PoolWorkerState::Connecting && worker.state != PoolWorkerState::Connecting {
                            worker.connecting_since = Instant::now();
                        }
                        worker.state = state;
                    }
                }
            }
            joined = workers.join_next(), if !workers.is_empty() => {
                match joined {
                    Some(Ok((worker_index, recycled))) => {
                        managed.remove(&worker_index);
                        if recycled { metrics.record_recycle(); }
                    }
                    Some(Err(error)) => {
                        metrics.set_last_error(Some(format!("split data worker task failed: {error}")));
                        managed.retain(|_, worker| worker.state != PoolWorkerState::Retiring);
                    }
                    None => {}
                }
            }
            _ = reconcile_tick.tick() => {}
        }

        let now = Instant::now();
        let counts = pool_counts(&managed);
        metrics.set_pool_counts(counts.idle, counts.busy);
        let demand = if demand_seen_at
            .is_some_and(|seen| now.saturating_duration_since(seen) <= DEMAND_HINT_TTL)
        {
            latest_demand.clone()
        } else {
            None
        };
        let active_streams = demand
            .as_ref()
            .map_or(0, |value| usize::from(value.active_streams));
        let queued_requests = demand
            .as_ref()
            .map_or(0, |value| usize::from(value.queued_requests));
        let desired_connections = demand
            .as_ref()
            .map_or(usize::from(initial_policy.min_connections), |value| {
                usize::from(value.desired_connections)
            });
        metrics.set_split_demand(active_streams, queued_requests, desired_connections);
        if counts.total > usize::from(initial_policy.min_connections)
            && counts.idle > 0
            && active_streams == 0
            && queued_requests == 0
        {
            idle_excess_since.get_or_insert(now);
        } else {
            idle_excess_since = None;
        }
        let idle_excess_elapsed = idle_excess_since.is_some_and(|started| {
            now.saturating_duration_since(started)
                >= Duration::from_secs(initial_policy.scale_down_delay_seconds)
        });
        let adjustment = multiplex_pool_adjustment(
            &initial_policy,
            MultiplexPoolSnapshot {
                connected_connections: counts.idle + counts.busy,
                connecting_connections: counts.connecting,
                active_streams,
                queued_requests,
                desired_connections,
                idle_connections: counts.idle,
                idle_excess_elapsed,
            },
        );
        let mut remaining = adjustment.retire;
        for worker in managed.values_mut() {
            if remaining == 0 {
                break;
            }
            if worker.state == PoolWorkerState::Idle {
                worker.state = PoolWorkerState::Retiring;
                let _ = worker.retire.send(true);
                remaining -= 1;
            }
        }
        for _ in 0..adjustment.spawn {
            spawn_split_data_worker(
                &mut workers,
                &mut managed,
                &mut next_worker_index,
                &config,
                &shutdown,
                &status_tx,
                &metrics,
                &event_tx,
                &session_id,
                routes.clone(),
                stream_limit,
            );
        }
    };

    for worker in managed.values() {
        let _ = worker.retire.send(true);
    }
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    metrics.set_pool_counts(0, 0);
    result
}

#[allow(clippy::too_many_arguments)]
fn spawn_split_data_worker(
    workers: &mut JoinSet<(usize, bool)>,
    managed: &mut HashMap<usize, ManagedWorker>,
    next_worker_index: &mut usize,
    config: &BuiltinTunnelConfig,
    shutdown: &watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: &Arc<BuiltinTunnelMetrics>,
    event_tx: &mpsc::Sender<WorkerEvent>,
    session_id: &str,
    routes: SplitRouteTable,
    stream_limit: u16,
) -> usize {
    let worker_index = *next_worker_index;
    *next_worker_index = next_worker_index.saturating_add(1);
    let (retire, retire_rx) = watch::channel(false);
    managed.insert(
        worker_index,
        ManagedWorker {
            state: PoolWorkerState::Connecting,
            connecting_since: Instant::now(),
            retire,
        },
    );
    workers.spawn(split_data_reconnect_loop(
        config.clone(),
        shutdown.clone(),
        retire_rx,
        status_tx.clone(),
        worker_index,
        metrics.clone(),
        event_tx.clone(),
        session_id.to_string(),
        routes,
        stream_limit,
    ));
    worker_index
}

#[allow(clippy::too_many_arguments)]
async fn split_data_reconnect_loop(
    config: BuiltinTunnelConfig,
    mut shutdown: watch::Receiver<bool>,
    mut retire: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    worker_index: usize,
    metrics: Arc<BuiltinTunnelMetrics>,
    event_tx: mpsc::Sender<WorkerEvent>,
    session_id: String,
    routes: SplitRouteTable,
    stream_limit: u16,
) -> (usize, bool) {
    let mut delay = INITIAL_RECONNECT_DELAY;
    let mut attempt = 0_u64;
    while !*shutdown.borrow() && !*retire.borrow() {
        let mut connected = false;
        let connection_id = format!(
            "{}-data-{worker_index}-{}",
            config.device_id,
            Uuid::new_v4()
        );
        let result = async {
            let connection = connect_split_data(&config, &connection_id, &session_id).await?;
            if connection.session_id != session_id {
                return Err("split data connection changed session id".to_string());
            }
            let initial_route_id = routes
                .read()
                .await
                .keys()
                .next()
                .copied()
                .ok_or_else(|| "split route table is empty".to_string())?;
            let multiplex = AuthenticatedMultiplexConnection {
                sink: connection.sink,
                stream: connection.stream,
                initial_policy: connection.initial_policy,
                route_id: initial_route_id,
                stream_limit,
                connection_id: connection.connection_id,
            };
            run_multiplex_connection(
                &config,
                multiplex,
                Some(routes.clone()),
                &mut shutdown,
                &mut retire,
                &status_tx,
                metrics.clone(),
                &mut connected,
                worker_index,
                &event_tx,
            )
            .await
        }
        .await;
        match result {
            Ok(WorkerConnectionExit::Shutdown) | Ok(WorkerConnectionExit::ScaleDown) => {
                return (worker_index, false)
            }
            Ok(WorkerConnectionExit::Recycle) => return (worker_index, true),
            Err(error) => {
                append_log(
                    &config.log_path,
                    &format!("split data worker {worker_index}: {error}"),
                );
                metrics.set_last_error(Some(error.clone()));
                let _ = status_tx.send(Err(error)).await;
            }
        }
        let _ = event_tx
            .send(WorkerEvent::State {
                worker_index,
                state: PoolWorkerState::Connecting,
            })
            .await;
        if connected {
            delay = next_reconnect_base(delay, true);
            attempt = 0;
        } else {
            attempt = attempt.saturating_add(1);
        }
        let sleep_for = reconnect_delay(delay, worker_index, attempt);
        tokio::select! {
            _ = shutdown.changed() => return (worker_index, false),
            _ = retire.changed() => return (worker_index, false),
            _ = sleep(sleep_for) => {}
        }
        delay = next_reconnect_base(delay, false);
    }
    (worker_index, false)
}

async fn run_legacy_worker_pool(
    config: BuiltinTunnelConfig,
    mut shutdown: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    initial_connection: Option<AuthenticatedTunnelConnection>,
) {
    let mut workers = JoinSet::<(usize, bool)>::new();
    let mut managed = HashMap::<usize, ManagedWorker>::new();
    let (event_tx, mut event_rx) = mpsc::channel(128);
    let (policy_tx, mut policy_rx) = watch::channel::<Option<WorkerPolicy>>(None);
    let mut next_worker_index = 0_usize;
    let mut idle_excess_since = None;
    let mut last_policy_revision = None;
    let mut last_scale_up_block = None;
    let mut demand_target = 0_usize;
    let mut demand_seen_at = None;
    let mut last_pressure_at = None;
    let mut reconcile_tick = interval(Duration::from_secs(1));
    reconcile_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let bootstrap_worker = spawn_managed_worker_with_initial(
        &mut workers,
        &mut managed,
        &mut next_worker_index,
        &config,
        &shutdown,
        &status_tx,
        &metrics,
        &event_tx,
        &policy_tx,
        initial_connection,
    );
    append_log(
        &config.log_path,
        &format!(
            "event=worker_spawn reason=bootstrap worker_indices={bootstrap_worker} before_total=0 after_total=1"
        ),
    );

    while !*shutdown.borrow() {
        tokio::select! {
            _ = shutdown.changed() => break,
            event = event_rx.recv() => {
                match event {
                    Some(WorkerEvent::State { worker_index, state }) => {
                        if let Some(worker) = managed.get_mut(&worker_index) {
                            let was_busy = worker.state == PoolWorkerState::Busy;
                            let was_connecting = worker.state == PoolWorkerState::Connecting;
                            if state == PoolWorkerState::Connecting && !was_connecting {
                                worker.connecting_since = Instant::now();
                            }
                            worker.state = state;
                            if state == PoolWorkerState::Busy
                                || (was_busy && state == PoolWorkerState::Idle)
                            {
                                last_pressure_at = Some(Instant::now());
                            }
                        }
                    }
                    Some(WorkerEvent::Demand(demand)) => {
                        demand_target = usize::from(demand.desired_workers);
                        demand_seen_at = Some(Instant::now());
                        if demand.queued_requests > 0 {
                            last_pressure_at = Some(Instant::now());
                        }
                        append_log(
                            &config.log_path,
                            &format!(
                                "event=worker_demand queued_requests={} oldest_queue_wait_ms={} desired_workers={} current_total={}",
                                demand.queued_requests,
                                demand.oldest_queue_wait_ms,
                                demand.desired_workers,
                                managed.len(),
                            ),
                        );
                    }
                    None => {}
                }
            }
            changed = policy_rx.changed() => {
                if changed.is_err() {
                    break;
                }
            }
            joined = workers.join_next(), if !workers.is_empty() => {
                match joined {
                    Some(Ok((worker_index, recycled))) => {
                        managed.remove(&worker_index);
                        if recycled {
                            metrics.record_recycle();
                        }
                    }
                    Some(Err(error)) => {
                        metrics.set_last_error(Some(format!("worker task failed: {error}")));
                        managed.retain(|_, worker| worker.state != PoolWorkerState::Retiring);
                    }
                    None => {}
                }
            }
            _ = reconcile_tick.tick() => {}
        }

        let Some(policy) = policy_rx.borrow().clone() else {
            continue;
        };
        if last_policy_revision != Some(policy.revision) {
            append_log(
                &config.log_path,
                &format!(
                    "event=worker_policy_applied service={} revision={} start_workers={} min_idle_workers={} max_idle_workers={} max_workers={} max_pending_requests={} worker_acquire_timeout_ms={} max_connecting_workers={} connecting_capacity_grace_ms={} scale_down_step={} burst_warm_workers={} burst_warm_seconds={} scale_down_delay_seconds={} max_requests_per_worker={} max_lifetime_seconds={}",
                    config.service.as_str(),
                    policy.revision,
                    policy.start_workers,
                    policy.min_idle_workers,
                    policy.max_idle_workers,
                    policy.max_workers,
                    policy.max_pending_requests,
                    policy.worker_acquire_timeout_ms,
                    policy.max_connecting_workers,
                    policy.connecting_capacity_grace_ms,
                    policy.scale_down_step,
                    policy.burst_warm_workers,
                    policy.burst_warm_seconds,
                    policy.scale_down_delay_seconds,
                    policy.max_requests_per_worker,
                    policy.max_lifetime_seconds,
                ),
            );
            last_policy_revision = Some(policy.revision);
            last_scale_up_block = None;
        }
        metrics.set_policy(&policy);
        let now = Instant::now();
        let connecting_grace = Duration::from_millis(policy.connecting_capacity_grace_ms);
        let counts = pool_counts(&managed);
        metrics.set_pool_counts(counts.idle, counts.busy);
        let demand_active = demand_seen_at
            .is_some_and(|seen| now.saturating_duration_since(seen) <= DEMAND_HINT_TTL);
        let desired_workers = if demand_active {
            demand_target.min(usize::from(policy.max_workers))
        } else {
            0
        };
        let effective_connecting = effective_connecting_workers(&managed, connecting_grace);
        let max_connecting = configured_max_connecting(&policy);
        let warm_floor = configured_burst_warm_floor(&policy);
        let warm_active = policy.burst_warm_seconds > 0
            && last_pressure_at.is_some_and(|seen| {
                now.saturating_duration_since(seen) < Duration::from_secs(policy.burst_warm_seconds)
            });
        let scale_down_floor = if warm_active {
            warm_floor
        } else {
            usize::from(policy.max_idle_workers)
        };
        if counts.total > scale_down_floor && counts.idle > 0 {
            idle_excess_since.get_or_insert(now);
        } else {
            idle_excess_since = None;
        }
        let idle_excess_elapsed = idle_excess_since.is_some_and(|started| {
            now.saturating_duration_since(started)
                >= Duration::from_secs(policy.scale_down_delay_seconds)
        });
        let adjustment = pool_adjustment(
            &policy,
            counts,
            effective_connecting,
            max_connecting,
            desired_workers,
            idle_excess_elapsed,
            scale_down_floor,
        );

        let blocked = scale_up_block(
            &policy,
            counts,
            effective_connecting,
            max_connecting,
            desired_workers,
            adjustment,
        );
        if blocked != last_scale_up_block {
            if let Some(reason) = blocked {
                append_log(
                    &config.log_path,
                    &format!(
                        "event=scale_up_blocked reason={} total={} connecting={} effective_connecting={} idle={} busy={} desired_workers={} min_idle_workers={} max_connecting_workers={} max_workers={} policy_revision={}",
                        reason.as_str(),
                        counts.total,
                        counts.connecting,
                        effective_connecting,
                        counts.idle,
                        counts.busy,
                        desired_workers,
                        policy.min_idle_workers,
                        max_connecting,
                        policy.max_workers,
                        policy.revision,
                    ),
                );
            }
            last_scale_up_block = blocked;
        }

        let mut remaining_retirements = adjustment.retire;
        let mut retired_worker_indices = Vec::with_capacity(adjustment.retire);
        for (worker_index, worker) in managed.iter_mut() {
            if remaining_retirements == 0 {
                break;
            }
            if worker.state == PoolWorkerState::Idle {
                worker.state = PoolWorkerState::Retiring;
                let _ = worker.retire.send(true);
                retired_worker_indices.push(*worker_index);
                remaining_retirements -= 1;
            }
        }
        if adjustment.retire > 0 {
            retired_worker_indices.sort_unstable();
            let after_total = counts.total.saturating_sub(retired_worker_indices.len());
            append_log(
                &config.log_path,
                &format!(
                    "event=scale_down reason={} requested={} retired={} worker_indices={} before_total={} after_total={} connecting={} idle={} busy={} scale_down_floor={} warm_active={} scale_down_step={} max_idle_workers={} max_workers={} policy_revision={}",
                    scale_down_reason(&policy, counts, idle_excess_elapsed, warm_active),
                    adjustment.retire,
                    retired_worker_indices.len(),
                    join_worker_indices(&retired_worker_indices),
                    counts.total,
                    after_total,
                    counts.connecting,
                    counts.idle,
                    counts.busy,
                    scale_down_floor,
                    warm_active,
                    policy.scale_down_step,
                    policy.max_idle_workers,
                    policy.max_workers,
                    policy.revision,
                ),
            );
            if after_total <= scale_down_floor {
                idle_excess_since = None;
            }
        }
        let mut spawned_worker_indices = Vec::with_capacity(adjustment.spawn);
        for _ in 0..adjustment.spawn {
            spawned_worker_indices.push(spawn_managed_worker(
                &mut workers,
                &mut managed,
                &mut next_worker_index,
                &config,
                &shutdown,
                &status_tx,
                &metrics,
                &event_tx,
                &policy_tx,
            ));
        }
        if adjustment.spawn > 0 {
            append_log(
                &config.log_path,
                &format!(
                    "event=scale_up reason={} requested={} spawned={} worker_indices={} before_total={} after_total={} connecting={} effective_connecting={} idle={} busy={} desired_workers={} start_workers={} min_idle_workers={} max_connecting_workers={} max_workers={} policy_revision={}",
                    scale_up_reason(&policy, counts, effective_connecting, desired_workers),
                    adjustment.spawn,
                    spawned_worker_indices.len(),
                    join_worker_indices(&spawned_worker_indices),
                    counts.total,
                    counts.total.saturating_add(spawned_worker_indices.len()),
                    counts.connecting,
                    effective_connecting,
                    counts.idle,
                    counts.busy,
                    desired_workers,
                    policy.start_workers,
                    policy.min_idle_workers,
                    max_connecting,
                    policy.max_workers,
                    policy.revision,
                ),
            );
        }
    }

    for worker in managed.values() {
        let _ = worker.retire.send(true);
    }
    workers.abort_all();
    while workers.join_next().await.is_some() {}
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolWorkerState {
    Connecting,
    Idle,
    Busy,
    Retiring,
}

struct ManagedWorker {
    state: PoolWorkerState,
    connecting_since: Instant,
    retire: watch::Sender<bool>,
}

enum WorkerEvent {
    State {
        worker_index: usize,
        state: PoolWorkerState,
    },
    Demand(WorkerDemand),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerConnectionExit {
    Shutdown,
    ScaleDown,
    Recycle,
}

#[allow(clippy::too_many_arguments)]
fn spawn_managed_worker(
    workers: &mut JoinSet<(usize, bool)>,
    managed: &mut HashMap<usize, ManagedWorker>,
    next_worker_index: &mut usize,
    config: &BuiltinTunnelConfig,
    shutdown: &watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: &Arc<BuiltinTunnelMetrics>,
    event_tx: &mpsc::Sender<WorkerEvent>,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
) -> usize {
    spawn_managed_worker_with_initial(
        workers,
        managed,
        next_worker_index,
        config,
        shutdown,
        status_tx,
        metrics,
        event_tx,
        policy_tx,
        None,
    )
}

#[allow(clippy::too_many_arguments)]
fn spawn_managed_worker_with_initial(
    workers: &mut JoinSet<(usize, bool)>,
    managed: &mut HashMap<usize, ManagedWorker>,
    next_worker_index: &mut usize,
    config: &BuiltinTunnelConfig,
    shutdown: &watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: &Arc<BuiltinTunnelMetrics>,
    event_tx: &mpsc::Sender<WorkerEvent>,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
    initial_connection: Option<AuthenticatedTunnelConnection>,
) -> usize {
    let worker_index = *next_worker_index;
    *next_worker_index = next_worker_index.saturating_add(1);
    let (retire, retire_rx) = watch::channel(false);
    managed.insert(
        worker_index,
        ManagedWorker {
            state: PoolWorkerState::Connecting,
            connecting_since: Instant::now(),
            retire,
        },
    );
    workers.spawn(worker_reconnect_loop(
        config.clone(),
        shutdown.clone(),
        retire_rx,
        status_tx.clone(),
        worker_index,
        metrics.clone(),
        event_tx.clone(),
        policy_tx.clone(),
        initial_connection,
    ));
    worker_index
}

fn pool_counts(workers: &HashMap<usize, ManagedWorker>) -> PoolCounts {
    let connecting = workers
        .values()
        .filter(|worker| worker.state == PoolWorkerState::Connecting)
        .count();
    let idle = workers
        .values()
        .filter(|worker| worker.state == PoolWorkerState::Idle)
        .count();
    let busy = workers
        .values()
        .filter(|worker| worker.state == PoolWorkerState::Busy)
        .count();
    PoolCounts {
        total: connecting + idle + busy,
        connecting,
        idle,
        busy,
    }
}

fn effective_connecting_workers(workers: &HashMap<usize, ManagedWorker>, grace: Duration) -> usize {
    if grace.is_zero() {
        return workers
            .values()
            .filter(|worker| worker.state == PoolWorkerState::Connecting)
            .count();
    }
    let now = Instant::now();
    workers
        .values()
        .filter(|worker| {
            worker.state == PoolWorkerState::Connecting
                && now.saturating_duration_since(worker.connecting_since) <= grace
        })
        .count()
}

#[allow(clippy::too_many_arguments)]
async fn worker_reconnect_loop(
    config: BuiltinTunnelConfig,
    mut shutdown: watch::Receiver<bool>,
    mut retire: watch::Receiver<bool>,
    status_tx: mpsc::Sender<Result<(), String>>,
    worker_index: usize,
    metrics: Arc<BuiltinTunnelMetrics>,
    event_tx: mpsc::Sender<WorkerEvent>,
    policy_tx: watch::Sender<Option<WorkerPolicy>>,
    mut preconnected: Option<AuthenticatedTunnelConnection>,
) -> (usize, bool) {
    let worker_id = format!("{}-{worker_index}-{}", config.client_id, Uuid::new_v4());
    let mut delay = INITIAL_RECONNECT_DELAY;
    let mut attempt = 0_u64;

    while !*shutdown.borrow() && !*retire.borrow() {
        let mut connected = false;
        match run_connected_worker_with_connection(
            &config,
            &worker_id,
            &mut shutdown,
            &mut retire,
            &status_tx,
            metrics.clone(),
            &mut connected,
            worker_index,
            &event_tx,
            &policy_tx,
            preconnected.take(),
        )
        .await
        {
            Ok(WorkerConnectionExit::Shutdown) => return (worker_index, false),
            Ok(WorkerConnectionExit::ScaleDown) => return (worker_index, false),
            Ok(WorkerConnectionExit::Recycle) => return (worker_index, true),
            Err(error) => {
                append_log(&config.log_path, &format!("worker {worker_index}: {error}"));
                metrics.set_last_error(Some(error.clone()));
                let _ = status_tx.send(Err(error)).await;
            }
        }

        let _ = event_tx
            .send(WorkerEvent::State {
                worker_index,
                state: PoolWorkerState::Connecting,
            })
            .await;

        if connected {
            delay = next_reconnect_base(delay, true);
            attempt = 0;
        } else {
            attempt = attempt.saturating_add(1);
        }
        let sleep_for = reconnect_delay(delay, worker_index, attempt);

        tokio::select! {
            _ = shutdown.changed() => return (worker_index, false),
            _ = retire.changed() => return (worker_index, false),
            _ = sleep(sleep_for) => {},
        }
        delay = next_reconnect_base(delay, false);
    }
    (worker_index, false)
}

#[allow(clippy::too_many_arguments)]
async fn run_connected_worker(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    connected: &mut bool,
    worker_index: usize,
    event_tx: &mpsc::Sender<WorkerEvent>,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
) -> Result<WorkerConnectionExit, String> {
    run_connected_worker_with_connection(
        config,
        worker_id,
        shutdown,
        retire,
        status_tx,
        metrics,
        connected,
        worker_index,
        event_tx,
        policy_tx,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_connected_worker_with_connection(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    connected: &mut bool,
    worker_index: usize,
    event_tx: &mpsc::Sender<WorkerEvent>,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
    preconnected: Option<AuthenticatedTunnelConnection>,
) -> Result<WorkerConnectionExit, String> {
    let connection_id = format!("{}-{worker_index}-{}", config.device_id, Uuid::new_v4());
    let connection = match preconnected {
        Some(connection) => connection,
        None => connect_authenticated_tunnel(config, worker_id, &connection_id).await?,
    };
    let AuthenticatedTunnelConnection::Legacy(AuthenticatedWorkerConnection {
        mut sink,
        mut stream,
        initial_policy,
    }) = connection
    else {
        let AuthenticatedTunnelConnection::Multiplex(connection) = connection else {
            unreachable!();
        };
        return run_multiplex_connection(
            config,
            connection,
            None,
            shutdown,
            retire,
            status_tx,
            metrics,
            connected,
            worker_index,
            event_tx,
        )
        .await;
    };
    policy_tx.send_replace(Some(initial_policy.clone()));
    send_control(&mut sink, &ControlMessage::Ready).await?;
    *connected = true;
    metrics.set_last_error(None);
    let _connected_guard = ConnectedWorkerGuard::new(metrics);
    let _ = status_tx.send(Ok(())).await;
    let _ = event_tx
        .send(WorkerEvent::State {
            worker_index,
            state: PoolWorkerState::Idle,
        })
        .await;
    append_log(&config.log_path, &format!("worker connected: {worker_id}"));

    let http = reqwest::Client::builder()
        .connect_timeout(LOCAL_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut heartbeat = HeartbeatTracker::new_at(Instant::now());
    let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
    heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let connected_at = Instant::now();
    let mut completed_requests = 0_u64;

    loop {
        if *shutdown.borrow() {
            close_client_websocket(&mut sink, &mut stream).await;
            return Ok(WorkerConnectionExit::Shutdown);
        }
        if *retire.borrow() {
            close_client_websocket(&mut sink, &mut stream).await;
            return Ok(WorkerConnectionExit::ScaleDown);
        }
        let Some(request) = receive_request(
            &mut sink,
            &mut stream,
            shutdown,
            retire,
            &mut heartbeat,
            &mut heartbeat_interval,
            policy_tx,
            event_tx,
            worker_index,
            connected_at,
            completed_requests,
        )
        .await?
        else {
            close_client_websocket(&mut sink, &mut stream).await;
            let policy = policy_tx
                .borrow()
                .clone()
                .unwrap_or_else(|| initial_policy.clone());
            return Ok(if *shutdown.borrow() {
                WorkerConnectionExit::Shutdown
            } else if worker_should_recycle(
                &policy,
                worker_index as u64,
                completed_requests,
                Instant::now().saturating_duration_since(connected_at),
            ) {
                WorkerConnectionExit::Recycle
            } else {
                WorkerConnectionExit::ScaleDown
            });
        };
        forward_request(
            config,
            &http,
            request,
            &mut sink,
            &mut stream,
            shutdown,
            &mut heartbeat,
            &mut heartbeat_interval,
        )
        .await?;
        completed_requests = completed_requests.saturating_add(1);
        let policy = policy_tx
            .borrow()
            .clone()
            .unwrap_or_else(|| initial_policy.clone());
        if worker_should_recycle(
            &policy,
            worker_index as u64,
            completed_requests,
            Instant::now().saturating_duration_since(connected_at),
        ) {
            close_client_websocket(&mut sink, &mut stream).await;
            return Ok(WorkerConnectionExit::Recycle);
        }
        send_control(&mut sink, &ControlMessage::Ready).await?;
        let _ = event_tx
            .send(WorkerEvent::State {
                worker_index,
                state: PoolWorkerState::Idle,
            })
            .await;
    }
}

struct PendingMultiplexRequest {
    route_id: u32,
    config: BuiltinTunnelConfig,
    method: String,
    path_and_query: String,
    headers: Vec<HeaderPair>,
    body: Vec<u8>,
    #[allow(dead_code)]
    demand: Option<MultiplexDemand>,
}

#[allow(clippy::too_many_arguments)]
async fn run_multiplex_connection(
    config: &BuiltinTunnelConfig,
    connection: AuthenticatedMultiplexConnection,
    shared_routes: Option<SplitRouteTable>,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    status_tx: &mpsc::Sender<Result<(), String>>,
    metrics: Arc<BuiltinTunnelMetrics>,
    connected: &mut bool,
    worker_index: usize,
    event_tx: &mpsc::Sender<WorkerEvent>,
) -> Result<WorkerConnectionExit, String> {
    let AuthenticatedMultiplexConnection {
        sink,
        mut stream,
        initial_policy: _,
        route_id,
        stream_limit,
        connection_id,
    } = connection;
    let writer = Arc::new(Mutex::new(sink));
    let http = reqwest::Client::builder()
        .connect_timeout(LOCAL_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| error.to_string())?;
    let mut pending = HashMap::<u64, PendingMultiplexRequest>::new();
    let mut cancellations = HashMap::<u64, (u32, watch::Sender<bool>)>::new();
    let mut tasks = JoinSet::<(u32, u64, Result<(), String>)>::new();
    let mut active_streams = 0_usize;
    let mut heartbeat = HeartbeatTracker::new_at(Instant::now());
    let mut heartbeat_interval = interval(HEARTBEAT_INTERVAL);
    heartbeat_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    *connected = true;
    metrics.set_last_error(None);
    let _connected_guard = ConnectedWorkerGuard::new(metrics);
    let _ = status_tx.send(Ok(())).await;
    let _ = event_tx
        .send(WorkerEvent::State {
            worker_index,
            state: PoolWorkerState::Idle,
        })
        .await;
    append_log(
        &config.log_path,
        &format!(
            "multiplex connection authenticated: connection_id={connection_id} route_id={route_id} stream_limit={stream_limit}"
        ),
    );

    loop {
        if *shutdown.borrow() {
            tasks.abort_all();
            return Ok(WorkerConnectionExit::Shutdown);
        }
        if *retire.borrow() && active_streams == 0 {
            tasks.abort_all();
            return Ok(WorkerConnectionExit::ScaleDown);
        }
        tokio::select! {
            _ = shutdown.changed() => {
                tasks.abort_all();
                return Ok(WorkerConnectionExit::Shutdown);
            }
            _ = retire.changed(), if !*retire.borrow() => {}
            _ = heartbeat_interval.tick() => {
                let mut sink = writer.lock().await;
                send_heartbeat(&mut sink, &heartbeat).await?;
            }
            joined = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(joined) = joined {
                    let (stream_route, stream_id, result) = joined.map_err(|error| error.to_string())?;
                    cancellations.remove(&stream_id);
                    active_streams = active_streams.saturating_sub(1);
                    if let Err(error) = result {
                        let mut sink = writer.lock().await;
                        let _ = protocol_io::send_multiplex_control(
                            &mut sink,
                            &MultiplexControlMessage::Error {
                                route_id: Some(stream_route),
                                stream_id: Some(stream_id),
                                message: error,
                            },
                        ).await;
                    }
                    if active_streams == 0 {
                        let _ = event_tx.send(WorkerEvent::State {
                            worker_index,
                            state: PoolWorkerState::Idle,
                        }).await;
                        if *retire.borrow() {
                            return Ok(WorkerConnectionExit::ScaleDown);
                        }
                    }
                }
            }
            incoming = stream.next() => {
                heartbeat.record_activity();
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        let control = decode_multiplex_control(text.as_str())?;
                        match control {
                            MultiplexControlMessage::RequestHead {
                                route_id: incoming_route,
                                stream_id,
                                method,
                                path_and_query,
                                headers,
                                demand,
                            } => {
                                if stream_id == 0 {
                                    return Err("multiplex request targeted an invalid stream".into());
                                }
                                let route_config = if let Some(routes) = shared_routes.as_ref() {
                                    routes.read().await.get(&incoming_route).cloned()
                                        .ok_or_else(|| "multiplex request targeted an unknown route".to_string())?
                                } else {
                                    if incoming_route != route_id { return Err("multiplex request targeted an unknown route".into()); }
                                    config.clone()
                                };
                                if pending.contains_key(&stream_id) || cancellations.contains_key(&stream_id) {
                                    return Err("duplicate multiplex stream id".into());
                                }
                                if pending.len().saturating_add(active_streams) >= usize::from(stream_limit) {
                                    return Err("multiplex stream capacity exceeded".into());
                                }
                                pending.insert(stream_id, PendingMultiplexRequest {
                                    route_id: incoming_route,
                                    config: route_config,
                                    method,
                                    path_and_query,
                                    headers,
                                    body: Vec::new(),
                                    demand,
                                });
                            }
                            MultiplexControlMessage::RequestEnd { route_id: incoming_route, stream_id } => {
                                let request = pending.remove(&stream_id)
                                    .ok_or_else(|| "multiplex request ended before its head".to_string())?;
                                if incoming_route != request.route_id {
                                    return Err("multiplex request end targeted a different route".into());
                                }
                                let (cancel_tx, cancel_rx) = watch::channel(false);
                                cancellations.insert(stream_id, (request.route_id, cancel_tx));
                                active_streams = active_streams.saturating_add(1);
                                if active_streams == 1 {
                                    let _ = event_tx.send(WorkerEvent::State {
                                        worker_index,
                                        state: PoolWorkerState::Busy,
                                    }).await;
                                }
                                let writer = writer.clone();
                                let route_id = request.route_id;
                                let route_config = request.config.clone();
                                let http = http.clone();
                                tasks.spawn(async move {
                                    let result = forward_multiplex_request(
                                        &route_config,
                                        &http,
                                        route_id,
                                        stream_id,
                                        request,
                                        writer,
                                        cancel_rx,
                                    ).await;
                                    (route_id, stream_id, result)
                                });
                            }
                            MultiplexControlMessage::Cancel { route_id: incoming_route, stream_id } => {
                                if pending.get(&stream_id).is_some_and(|request| request.route_id == incoming_route) {
                                    pending.remove(&stream_id);
                                }
                                if let Some((active_route, cancel)) = cancellations.get(&stream_id) {
                                    if *active_route == incoming_route {
                                        let _ = cancel.send(true);
                                    }
                                }
                            }
                            MultiplexControlMessage::Error { message, .. } => return Err(message),
                            MultiplexControlMessage::RouteUnregister { route_id: incoming_route }
                                if shared_routes.is_none() && incoming_route == route_id => return Ok(WorkerConnectionExit::ScaleDown),
                            MultiplexControlMessage::RouteUnregister { .. } if shared_routes.is_some() => {},
                            _ => return Err("unexpected multiplex control message".into()),
                        }
                    }
                    Some(Ok(Message::Binary(frame))) => {
                        let frame = decode_multiplex_binary_frame(frame.as_ref())?;
                        if frame.kind != MultiplexBinaryFrameKind::RequestBody {
                            return Err("unexpected multiplex binary frame".into());
                        }
                        let request = pending.get_mut(&frame.stream_id)
                            .ok_or_else(|| "multiplex request body arrived without a head".to_string())?;
                        if frame.route_id != request.route_id {
                            return Err("multiplex request body targeted a different route".into());
                        }
                        if request.body.len().saturating_add(frame.payload.len()) > MAX_REQUEST_BODY_BYTES {
                            return Err("request body exceeds built-in tunnel limit".into());
                        }
                        request.body.extend_from_slice(&frame.payload);
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        let mut sink = writer.lock().await;
                        sink.send(Message::Pong(payload)).await.map_err(|error| error.to_string())?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) | None => {
                        tasks.abort_all();
                        return Err("websocket closed".into());
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        tasks.abort_all();
                        return Err(error.to_string());
                    }
                }
            }
        }
    }
}

async fn wait_for_multiplex_cancel(cancelled: &mut watch::Receiver<bool>) {
    loop {
        if *cancelled.borrow() {
            return;
        }
        if cancelled.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

async fn forward_multiplex_request(
    config: &BuiltinTunnelConfig,
    http: &reqwest::Client,
    route_id: u32,
    stream_id: u64,
    request: PendingMultiplexRequest,
    writer: Arc<Mutex<ClientSink>>,
    mut cancelled: watch::Receiver<bool>,
) -> Result<(), String> {
    let request = IncomingRequest {
        request_id: format!("v5-{route_id}-{stream_id}"),
        method: request.method,
        path_and_query: request.path_and_query,
        headers: request.headers,
        body: request.body,
    };
    let (_, builder) = prepare_local_request(config, http, request)?;
    let response_future = builder.send();
    tokio::pin!(response_future);
    let response = tokio::select! {
        _ = wait_for_multiplex_cancel(&mut cancelled) => return Ok(()),
        response = &mut response_future => response.map_err(|error| error.to_string())?,
    };
    {
        let mut sink = writer.lock().await;
        protocol_io::send_multiplex_control(
            &mut sink,
            &MultiplexControlMessage::ResponseHead {
                route_id,
                stream_id,
                status: response.status().as_u16(),
                headers: response_headers(response.headers()),
            },
        )
        .await?;
    }

    let mut body = response.bytes_stream();
    loop {
        tokio::select! {
            _ = wait_for_multiplex_cancel(&mut cancelled) => return Ok(()),
            chunk = body.next() => match chunk {
                Some(Ok(chunk)) => {
                    let frame = encode_multiplex_binary_frame(
                        MultiplexBinaryFrameKind::ResponseBody,
                        route_id,
                        stream_id,
                        chunk.as_ref(),
                    )?;
                    let mut sink = writer.lock().await;
                    sink.send(Message::Binary(frame.into())).await.map_err(|error| error.to_string())?;
                }
                Some(Err(error)) => return Err(error.to_string()),
                None => break,
            }
        }
    }
    let mut sink = writer.lock().await;
    protocol_io::send_multiplex_control(
        &mut sink,
        &MultiplexControlMessage::ResponseEnd {
            route_id,
            stream_id,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn receive_request(
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    heartbeat: &mut HeartbeatTracker,
    heartbeat_interval: &mut Interval,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
    event_tx: &mpsc::Sender<WorkerEvent>,
    worker_index: usize,
    connected_at: Instant,
    completed_requests: u64,
) -> Result<Option<IncomingRequest>, String> {
    let Some(head) = receive_live_control(
        sink,
        stream,
        shutdown,
        retire,
        heartbeat,
        heartbeat_interval,
        policy_tx,
        connected_at,
        worker_index as u64,
        completed_requests,
    )
    .await?
    else {
        return Ok(None);
    };
    let ControlMessage::RequestHead {
        request_id,
        method,
        path_and_query,
        headers,
        demand,
    } = head
    else {
        return Err("expected request_head".into());
    };
    if let Some(demand) = demand {
        let _ = event_tx.send(WorkerEvent::Demand(demand)).await;
    }
    let _ = event_tx
        .send(WorkerEvent::State {
            worker_index,
            state: PoolWorkerState::Busy,
        })
        .await;

    let mut body = Vec::new();
    loop {
        let message = tokio::select! {
            _ = shutdown.changed() => return Err("tunnel stopped".into()),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
                continue;
            },
            message = stream.next() => message,
        };
        heartbeat.record_activity();
        match message {
            Some(Ok(Message::Binary(chunk))) => {
                if body.len().saturating_add(chunk.len()) > MAX_REQUEST_BODY_BYTES {
                    send_control(
                        sink,
                        &ControlMessage::Error {
                            request_id: Some(request_id.clone()),
                            message: "request body exceeds built-in tunnel limit".into(),
                        },
                    )
                    .await?;
                    return Err("request body exceeds built-in tunnel limit".into());
                }
                body.extend_from_slice(&chunk);
            }
            Some(Ok(Message::Text(text))) => {
                let control = decode_control(text.as_str())?;
                match control {
                    ControlMessage::RequestEnd { request_id: end_id } if end_id == request_id => {
                        break
                    }
                    ControlMessage::Cancel {
                        request_id: cancel_id,
                    } if cancel_id == request_id => return Err("request cancelled".into()),
                    _ => return Err("unexpected control message while receiving request".into()),
                }
            }
            Some(Ok(Message::Ping(payload))) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => {}
            Some(Err(error)) => return Err(error.to_string()),
        }
    }

    Ok(Some(IncomingRequest {
        request_id,
        method,
        path_and_query,
        headers,
        body,
    }))
}

async fn forward_request(
    config: &BuiltinTunnelConfig,
    http: &reqwest::Client,
    request: IncomingRequest,
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    shutdown: &mut watch::Receiver<bool>,
    heartbeat: &mut HeartbeatTracker,
    heartbeat_interval: &mut Interval,
) -> Result<(), String> {
    let (request_id, builder) = prepare_local_request(config, http, request)?;

    let response_future = builder.send();
    tokio::pin!(response_future);
    let response = loop {
        tokio::select! {
            _ = shutdown.changed() => return Err("tunnel stopped".into()),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
            },
            response = &mut response_future => break response.map_err(|error| error.to_string())?,
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heartbeat.record_activity();
                    let control = decode_control(text.as_str())?;
                    if matches!(control, ControlMessage::Cancel { request_id: ref id } if id == &request_id) {
                        return Ok(());
                    }
                    return Err("unexpected control message while waiting for local response".into());
                }
                Some(Ok(Message::Ping(payload))) => sink.send(Message::Pong(payload))
                    .await.map_err(|error| error.to_string()).map(|_| heartbeat.record_activity())?,
                Some(Ok(Message::Pong(_))) => heartbeat.record_activity(),
                Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.to_string()),
            }
        }
    };

    let status = response.status().as_u16();
    let headers = response_headers(response.headers());
    send_control(
        sink,
        &ControlMessage::ResponseHead {
            request_id: request_id.clone(),
            status,
            headers,
        },
    )
    .await?;

    let mut body = response.bytes_stream();
    loop {
        tokio::select! {
            _ = shutdown.changed() => return Err("tunnel stopped".into()),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
            },
            chunk = body.next() => match chunk {
                Some(Ok(chunk)) => sink.send(Message::Binary(chunk))
                    .await.map_err(|error| error.to_string())?,
                Some(Err(error)) => {
                    send_control(sink, &ControlMessage::Error {
                        request_id: Some(request_id.clone()),
                        message: error.to_string(),
                    }).await?;
                    return Ok(());
                }
                None => break,
            },
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    heartbeat.record_activity();
                    let control = decode_control(text.as_str())?;
                    if matches!(control, ControlMessage::Cancel { request_id: ref id } if id == &request_id) {
                        return Ok(());
                    }
                    return Err("unexpected control message while streaming local response".into());
                }
                Some(Ok(Message::Ping(payload))) => sink.send(Message::Pong(payload))
                    .await.map_err(|error| error.to_string()).map(|_| heartbeat.record_activity())?,
                Some(Ok(Message::Pong(_))) => heartbeat.record_activity(),
                Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
                Some(Ok(_)) => {},
                Some(Err(error)) => return Err(error.to_string()),
            }
        }
    }
    send_control(
        sink,
        &ControlMessage::ResponseEnd {
            request_id: request_id.clone(),
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn receive_live_control(
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    shutdown: &mut watch::Receiver<bool>,
    retire: &mut watch::Receiver<bool>,
    heartbeat: &mut HeartbeatTracker,
    heartbeat_interval: &mut Interval,
    policy_tx: &watch::Sender<Option<WorkerPolicy>>,
    connected_at: Instant,
    worker_seed: u64,
    completed_requests: u64,
) -> Result<Option<ControlMessage>, String> {
    loop {
        if *shutdown.borrow() || *retire.borrow() {
            return Ok(None);
        }
        let policy = policy_tx.borrow().clone();
        if policy.as_ref().is_some_and(|policy| {
            worker_should_recycle(
                policy,
                worker_seed,
                completed_requests,
                Instant::now().saturating_duration_since(connected_at),
            )
        }) {
            return Ok(None);
        }
        let recycle_at = policy.as_ref().and_then(|policy| {
            let seconds = jittered_limit(
                policy.max_lifetime_seconds,
                worker_seed,
                policy.recycle_jitter_percent,
            );
            (seconds != 0).then(|| connected_at + Duration::from_secs(seconds))
        });
        let recycle_timer = async move {
            match recycle_at {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(recycle_timer);
        let message = tokio::select! {
            _ = shutdown.changed() => return Ok(None),
            _ = retire.changed() => return Ok(None),
            _ = &mut recycle_timer => return Ok(None),
            _ = heartbeat_interval.tick() => {
                send_heartbeat(sink, heartbeat).await?;
                continue;
            },
            message = stream.next() => message,
        };
        heartbeat.record_activity();
        match message {
            Some(Ok(Message::Text(text))) => {
                let control = decode_control(text.as_str())?;
                if let ControlMessage::PolicyUpdate { worker_policy } = control {
                    worker_policy.validate()?;
                    policy_tx.send_replace(Some(worker_policy));
                    continue;
                }
                return Ok(Some(control));
            }
            Some(Ok(Message::Ping(payload))) => sink
                .send(Message::Pong(payload))
                .await
                .map_err(|error| error.to_string())?,
            Some(Ok(Message::Pong(_))) => {}
            Some(Ok(Message::Close(_))) | None => return Err("websocket closed".into()),
            Some(Ok(_)) => return Err("expected text control message".into()),
            Some(Err(error)) => return Err(error.to_string()),
        }
    }
}

fn local_connect_host(bind_address: &str) -> String {
    match bind_address.trim() {
        "" | "0.0.0.0" | "::" | "[::]" => "127.0.0.1".into(),
        value if value.contains(':') && !value.starts_with('[') => format!("[{value}]"),
        value => value.to_string(),
    }
}

fn append_log(path: &Path, line: &str) {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|value| value.as_secs())
            .unwrap_or_default();
        let _ = writeln!(file, "[{timestamp}] {line}");
    }
}

#[cfg(test)]
mod tests;
