use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use coding_tools_tunnel_protocol::{
    auth_signing_payload, device_session_auth_signing_payload,
    multiplex_server_ack_signing_payload, multiplex_server_challenge_signing_payload,
    server_ack_signing_payload, server_challenge_signing_payload, split_server_ack_signing_payload,
    split_session_auth_signing_payload, ClientHello, ControlMessage, DeviceAuthProof,
    DeviceSessionAuthProof, DeviceSessionHello, MultiplexConnectionPolicy, MultiplexConnectionRole,
    MultiplexControlMessage, MultiplexRouteId, SplitSessionAuthProof, SplitSessionHello,
    CLIENT_ID_HEADER, CONNECTION_ID_HEADER, DEVICE_ID_HEADER, MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
    MULTIPLEX_DATA_WS_SUBPROTOCOL, MULTIPLEX_PROTOCOL_VERSION, MULTIPLEX_WS_SUBPROTOCOL,
    PROTOCOL_VERSION, SERVICE_HEADER, SESSION_ID_HEADER, WORKER_ID_HEADER, WS_SUBPROTOCOL,
};
use ed25519_dalek::{Signature, Signer, Verifier};
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;
use uuid::Uuid;

use super::protocol_io::{
    receive_control, receive_multiplex_control, send_control, send_multiplex_control, ClientSink,
    ClientStream, ClientWebSocket,
};
use super::{BuiltinTunnelConfig, WEBSOCKET_CONNECT_TIMEOUT};

pub(super) struct AuthenticatedWorkerConnection {
    pub(super) sink: ClientSink,
    pub(super) stream: ClientStream,
    pub(super) initial_policy: coding_tools_tunnel_protocol::WorkerPolicy,
}

pub(super) struct AuthenticatedMultiplexConnection {
    pub(super) sink: ClientSink,
    pub(super) stream: ClientStream,
    pub(super) initial_policy: MultiplexConnectionPolicy,
    pub(super) route_id: MultiplexRouteId,
    pub(super) stream_limit: u16,
    pub(super) connection_id: String,
}

pub(super) struct AuthenticatedSplitControlConnection {
    pub(super) sink: ClientSink,
    pub(super) stream: ClientStream,
    pub(super) initial_policy: MultiplexConnectionPolicy,
    pub(super) session_id: String,
    pub(super) route_id: MultiplexRouteId,
    pub(super) stream_limit: u16,
    pub(super) connection_id: String,
}

pub(super) struct AuthenticatedSplitDataConnection {
    pub(super) sink: ClientSink,
    pub(super) stream: ClientStream,
    pub(super) initial_policy: MultiplexConnectionPolicy,
    pub(super) session_id: String,
    pub(super) connection_id: String,
}

pub(super) enum SplitControlNegotiation {
    Split(AuthenticatedSplitControlConnection),
    Fallback(AuthenticatedTunnelConnection),
}

pub(super) enum AuthenticatedTunnelConnection {
    Legacy(AuthenticatedWorkerConnection),
    Multiplex(AuthenticatedMultiplexConnection),
}

pub(super) async fn connect_authenticated_worker(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
) -> Result<AuthenticatedWorkerConnection, String> {
    let opened = open_socket(config, worker_id, None, None, WS_SUBPROTOCOL).await?;
    authenticate_legacy_socket(config, worker_id, opened).await
}

struct OpenedSocket {
    socket: ClientWebSocket,
    selected_protocol: String,
}

async fn open_socket(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    connection_id: Option<&str>,
    session_id: Option<&str>,
    protocols: &str,
) -> Result<OpenedSocket, String> {
    let mut request = config
        .websocket_url
        .clone()
        .into_client_request()
        .map_err(|error| error.to_string())?;
    request.headers_mut().insert(
        CLIENT_ID_HEADER,
        config
            .client_id
            .parse()
            .map_err(|error| format!("invalid client id header: {error}"))?,
    );
    request.headers_mut().insert(
        SERVICE_HEADER,
        config
            .service
            .as_str()
            .parse()
            .map_err(|error| format!("invalid service header: {error}"))?,
    );
    request.headers_mut().insert(
        DEVICE_ID_HEADER,
        config
            .device_id
            .parse()
            .map_err(|error| format!("invalid device id header: {error}"))?,
    );
    request.headers_mut().insert(
        WORKER_ID_HEADER,
        worker_id
            .parse()
            .map_err(|error| format!("invalid worker id header: {error}"))?,
    );
    if let Some(connection_id) = connection_id {
        request.headers_mut().insert(
            CONNECTION_ID_HEADER,
            connection_id
                .parse()
                .map_err(|error| format!("invalid connection id header: {error}"))?,
        );
    }
    if let Some(session_id) = session_id {
        request.headers_mut().insert(
            SESSION_ID_HEADER,
            session_id
                .parse()
                .map_err(|error| format!("invalid session id header: {error}"))?,
        );
    }
    request.headers_mut().insert(
        SEC_WEBSOCKET_PROTOCOL,
        protocols
            .parse()
            .map_err(|error| format!("invalid tunnel subprotocol: {error}"))?,
    );

    let (socket, response) = timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| "WSS connection timed out".to_string())?
        .map_err(|error| error.to_string())?;
    let selected_protocol = response
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| "server did not select a tunnel subprotocol".to_string())?
        .to_string();
    if !protocols
        .split(',')
        .any(|offered| offered.trim() == selected_protocol)
    {
        return Err(format!(
            "server selected unsupported tunnel subprotocol {selected_protocol}"
        ));
    }
    Ok(OpenedSocket {
        socket,
        selected_protocol,
    })
}

async fn authenticate_legacy_socket(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    opened: OpenedSocket,
) -> Result<AuthenticatedWorkerConnection, String> {
    if opened.selected_protocol != WS_SUBPROTOCOL {
        return Err(format!("server did not accept {WS_SUBPROTOCOL}"));
    }
    let (mut sink, mut stream) = opened.socket.split();
    let (nonce, expires_at_unix_ms, server_id, server_signature) =
        match receive_control(&mut sink, &mut stream).await? {
            ControlMessage::Challenge {
                nonce,
                expires_at_unix_ms,
                server_id,
                server_signature,
            } => (nonce, expires_at_unix_ms, server_id, server_signature),
            ControlMessage::Error { message, .. } => return Err(message),
            _ => return Err("server did not issue a device authentication challenge".into()),
        };
    if unix_ms() > expires_at_unix_ms {
        return Err("server authentication challenge already expired".into());
    }
    if server_id != config.server_id {
        return Err("tunnel server identity does not match the enrolled server".into());
    }
    verify_server_signature(
        &config.server_verifying_key,
        &server_challenge_signing_payload(
            &nonce,
            expires_at_unix_ms,
            &server_id,
            &config.device_id,
            &config.client_id,
            config.service,
            worker_id,
        ),
        &server_signature,
        "challenge",
    )?;

    let mut proof = DeviceAuthProof {
        hello: ClientHello {
            protocol_version: PROTOCOL_VERSION,
            client_id: config.client_id.clone(),
            service: config.service,
            worker_id: worker_id.to_string(),
        },
        device_id: config.device_id.clone(),
        signature: String::new(),
    };
    proof.signature = URL_SAFE_NO_PAD.encode(
        config
            .signing_key
            .sign(&auth_signing_payload(&nonce, &proof))
            .to_bytes(),
    );
    send_control(&mut sink, &ControlMessage::Authenticate(proof.clone())).await?;

    let initial_policy = match receive_control(&mut sink, &mut stream).await? {
        ControlMessage::HelloAck {
            protocol_version,
            worker_policy,
            server_id,
            server_signature,
        } if protocol_version == PROTOCOL_VERSION => {
            if server_id != config.server_id {
                return Err(
                    "tunnel server acknowledgement identity changed during authentication".into(),
                );
            }
            verify_server_signature(
                &config.server_verifying_key,
                &server_ack_signing_payload(&nonce, &server_id, &proof, &worker_policy),
                &server_signature,
                "acknowledgement",
            )?;
            worker_policy
        }
        ControlMessage::Error { message, .. } => return Err(message),
        _ => return Err("server did not acknowledge tunnel device authentication".into()),
    };
    initial_policy.validate()?;
    Ok(AuthenticatedWorkerConnection {
        sink,
        stream,
        initial_policy,
    })
}

async fn authenticate_multiplex_socket(
    config: &BuiltinTunnelConfig,
    connection_id: &str,
    opened: OpenedSocket,
) -> Result<AuthenticatedMultiplexConnection, String> {
    if opened.selected_protocol != MULTIPLEX_WS_SUBPROTOCOL {
        return Err(format!("server did not accept {MULTIPLEX_WS_SUBPROTOCOL}"));
    }
    let (mut sink, mut stream) = opened.socket.split();
    let (nonce, expires_at_unix_ms, server_id, server_signature) =
        match receive_multiplex_control(&mut sink, &mut stream).await? {
            MultiplexControlMessage::Challenge {
                nonce,
                expires_at_unix_ms,
                server_id,
                server_signature,
            } => (nonce, expires_at_unix_ms, server_id, server_signature),
            MultiplexControlMessage::Error { message, .. } => return Err(message),
            _ => {
                return Err(
                    "server did not issue a multiplex device authentication challenge".into(),
                )
            }
        };
    if unix_ms() > expires_at_unix_ms {
        return Err("server authentication challenge already expired".into());
    }
    if server_id != config.server_id {
        return Err("tunnel server identity does not match the enrolled server".into());
    }
    verify_server_signature(
        &config.server_verifying_key,
        &multiplex_server_challenge_signing_payload(
            &nonce,
            expires_at_unix_ms,
            &server_id,
            &config.device_id,
            connection_id,
        ),
        &server_signature,
        "multiplex challenge",
    )?;

    let mut proof = DeviceSessionAuthProof {
        hello: DeviceSessionHello {
            protocol_version: MULTIPLEX_PROTOCOL_VERSION,
            connection_id: connection_id.to_string(),
        },
        device_id: config.device_id.clone(),
        signature: String::new(),
    };
    proof.signature = URL_SAFE_NO_PAD.encode(
        config
            .signing_key
            .sign(&device_session_auth_signing_payload(&nonce, &proof))
            .to_bytes(),
    );
    send_multiplex_control(
        &mut sink,
        &MultiplexControlMessage::Authenticate(proof.clone()),
    )
    .await?;

    let initial_policy = match receive_multiplex_control(&mut sink, &mut stream).await? {
        MultiplexControlMessage::HelloAck {
            protocol_version,
            connection_policy,
            server_id,
            server_signature,
        } if protocol_version == MULTIPLEX_PROTOCOL_VERSION => {
            if server_id != config.server_id {
                return Err(
                    "tunnel server acknowledgement identity changed during authentication".into(),
                );
            }
            verify_server_signature(
                &config.server_verifying_key,
                &multiplex_server_ack_signing_payload(
                    &nonce,
                    &server_id,
                    &proof,
                    &connection_policy,
                ),
                &server_signature,
                "multiplex acknowledgement",
            )?;
            connection_policy
        }
        MultiplexControlMessage::Error { message, .. } => return Err(message),
        _ => return Err("server did not acknowledge multiplex device authentication".into()),
    };
    initial_policy.validate()?;

    let registration_id = Uuid::new_v4().to_string();
    send_multiplex_control(
        &mut sink,
        &MultiplexControlMessage::RouteRegister {
            registration_id: registration_id.clone(),
            client_id: config.client_id.clone(),
            service: config.service,
        },
    )
    .await?;
    let (route_id, stream_limit) = match receive_multiplex_control(&mut sink, &mut stream).await? {
        MultiplexControlMessage::RouteRegistered {
            registration_id: accepted,
            route_id,
            stream_limit,
        } if accepted == registration_id => (route_id, stream_limit),
        MultiplexControlMessage::Error { message, .. } => return Err(message),
        _ => return Err("server did not register the multiplex route".into()),
    };
    if route_id == 0 || stream_limit == 0 || stream_limit > initial_policy.streams_per_connection {
        return Err("server returned an invalid multiplex route capacity".into());
    }

    Ok(AuthenticatedMultiplexConnection {
        sink,
        stream,
        initial_policy,
        route_id,
        stream_limit,
        connection_id: connection_id.to_string(),
    })
}

async fn authenticate_split_socket(
    config: &BuiltinTunnelConfig,
    connection_id: &str,
    session_id: Option<&str>,
    role: MultiplexConnectionRole,
    opened: OpenedSocket,
) -> Result<(ClientSink, ClientStream, MultiplexConnectionPolicy, String), String> {
    let expected_protocol = match role {
        MultiplexConnectionRole::Control => MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
        MultiplexConnectionRole::Data => MULTIPLEX_DATA_WS_SUBPROTOCOL,
    };
    if opened.selected_protocol != expected_protocol {
        return Err(format!("server did not accept {expected_protocol}"));
    }
    let (mut sink, mut stream) = opened.socket.split();
    let (nonce, expires_at_unix_ms, server_id, server_signature) =
        match receive_multiplex_control(&mut sink, &mut stream).await? {
            MultiplexControlMessage::Challenge {
                nonce,
                expires_at_unix_ms,
                server_id,
                server_signature,
            } => (nonce, expires_at_unix_ms, server_id, server_signature),
            MultiplexControlMessage::Error { message, .. } => return Err(message),
            _ => return Err("server did not issue a split tunnel authentication challenge".into()),
        };
    if unix_ms() > expires_at_unix_ms {
        return Err("server authentication challenge already expired".into());
    }
    if server_id != config.server_id {
        return Err("tunnel server identity does not match the enrolled server".into());
    }
    verify_server_signature(
        &config.server_verifying_key,
        &multiplex_server_challenge_signing_payload(
            &nonce,
            expires_at_unix_ms,
            &server_id,
            &config.device_id,
            connection_id,
        ),
        &server_signature,
        "split tunnel challenge",
    )?;
    let mut proof = SplitSessionAuthProof {
        hello: SplitSessionHello {
            protocol_version: MULTIPLEX_PROTOCOL_VERSION,
            connection_role: role,
            connection_id: connection_id.to_string(),
            session_id: session_id.map(str::to_string),
        },
        device_id: config.device_id.clone(),
        signature: String::new(),
    };
    proof.signature = URL_SAFE_NO_PAD.encode(
        config
            .signing_key
            .sign(&split_session_auth_signing_payload(&nonce, &proof))
            .to_bytes(),
    );
    send_multiplex_control(
        &mut sink,
        &MultiplexControlMessage::SessionAuthenticate(proof.clone()),
    )
    .await?;
    let (initial_policy, accepted_session_id) =
        match receive_multiplex_control(&mut sink, &mut stream).await? {
            MultiplexControlMessage::SessionReady {
                protocol_version,
                connection_role,
                session_id,
                connection_policy,
                server_id,
                server_signature,
            } if protocol_version == MULTIPLEX_PROTOCOL_VERSION && connection_role == role => {
                if server_id != config.server_id {
                    return Err(
                        "split tunnel acknowledgement identity changed during authentication"
                            .into(),
                    );
                }
                if let Some(expected) = proof.hello.session_id.as_deref() {
                    if session_id != expected {
                        return Err("split tunnel acknowledgement changed the session id".into());
                    }
                }
                verify_server_signature(
                    &config.server_verifying_key,
                    &split_server_ack_signing_payload(
                        &nonce,
                        &server_id,
                        &proof,
                        &session_id,
                        &connection_policy,
                    ),
                    &server_signature,
                    "split tunnel acknowledgement",
                )?;
                (connection_policy, session_id)
            }
            MultiplexControlMessage::Error { message, .. } => return Err(message),
            _ => return Err("server did not acknowledge split tunnel authentication".into()),
        };
    initial_policy.validate()?;
    Ok((sink, stream, initial_policy, accepted_session_id))
}

async fn register_split_control_route(
    config: &BuiltinTunnelConfig,
    sink: &mut ClientSink,
    stream: &mut ClientStream,
    initial_policy: &MultiplexConnectionPolicy,
) -> Result<(MultiplexRouteId, u16), String> {
    let registration_id = Uuid::new_v4().to_string();
    send_multiplex_control(
        sink,
        &MultiplexControlMessage::RouteRegister {
            registration_id: registration_id.clone(),
            client_id: config.client_id.clone(),
            service: config.service,
        },
    )
    .await?;
    loop {
        match receive_multiplex_control(sink, stream).await? {
            MultiplexControlMessage::RouteRegistered {
                registration_id: accepted,
                route_id,
                stream_limit,
            } if accepted == registration_id => {
                if route_id == 0
                    || stream_limit == 0
                    || stream_limit > initial_policy.streams_per_connection
                {
                    return Err("server returned an invalid split route capacity".into());
                }
                return Ok((route_id, stream_limit));
            }
            MultiplexControlMessage::CapacityUpdate { .. } => {}
            MultiplexControlMessage::Error { message, .. } => return Err(message),
            _ => return Err("server did not register the split tunnel route".into()),
        }
    }
}

pub(super) async fn connect_split_control_or_fallback(
    config: &BuiltinTunnelConfig,
    connection_id: &str,
) -> Result<SplitControlNegotiation, String> {
    let offered =
        format!("{MULTIPLEX_CONTROL_WS_SUBPROTOCOL}, {MULTIPLEX_WS_SUBPROTOCOL}, {WS_SUBPROTOCOL}");
    let opened = open_socket(config, connection_id, Some(connection_id), None, &offered).await?;
    match opened.selected_protocol.as_str() {
        MULTIPLEX_CONTROL_WS_SUBPROTOCOL => {
            let (mut sink, mut stream, initial_policy, session_id) = authenticate_split_socket(
                config,
                connection_id,
                None,
                MultiplexConnectionRole::Control,
                opened,
            )
            .await?;
            let (route_id, stream_limit) =
                register_split_control_route(config, &mut sink, &mut stream, &initial_policy)
                    .await?;
            Ok(SplitControlNegotiation::Split(
                AuthenticatedSplitControlConnection {
                    sink,
                    stream,
                    initial_policy,
                    session_id,
                    route_id,
                    stream_limit,
                    connection_id: connection_id.to_string(),
                },
            ))
        }
        MULTIPLEX_WS_SUBPROTOCOL => authenticate_multiplex_socket(config, connection_id, opened)
            .await
            .map(AuthenticatedTunnelConnection::Multiplex)
            .map(SplitControlNegotiation::Fallback),
        WS_SUBPROTOCOL => authenticate_legacy_socket(config, connection_id, opened)
            .await
            .map(AuthenticatedTunnelConnection::Legacy)
            .map(SplitControlNegotiation::Fallback),
        selected => Err(format!(
            "server selected unsupported tunnel subprotocol {selected}"
        )),
    }
}

pub(super) async fn reconnect_split_control(
    config: &BuiltinTunnelConfig,
    connection_id: &str,
    session_id: &str,
    route_id: MultiplexRouteId,
    stream_limit: u16,
) -> Result<AuthenticatedSplitControlConnection, String> {
    let opened = open_socket(
        config,
        connection_id,
        Some(connection_id),
        Some(session_id),
        MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
    )
    .await?;
    if opened.selected_protocol != MULTIPLEX_CONTROL_WS_SUBPROTOCOL {
        return Err("server did not accept split control reconnect".into());
    }
    let (sink, stream, initial_policy, accepted_session_id) = authenticate_split_socket(
        config,
        connection_id,
        Some(session_id),
        MultiplexConnectionRole::Control,
        opened,
    )
    .await?;
    if stream_limit == 0 || stream_limit > initial_policy.streams_per_connection {
        return Err("split control reconnect changed the route capacity".into());
    }
    Ok(AuthenticatedSplitControlConnection {
        sink,
        stream,
        initial_policy,
        session_id: accepted_session_id,
        route_id,
        stream_limit,
        connection_id: connection_id.to_string(),
    })
}

pub(super) async fn connect_split_data(
    config: &BuiltinTunnelConfig,
    connection_id: &str,
    session_id: &str,
) -> Result<AuthenticatedSplitDataConnection, String> {
    let opened = open_socket(
        config,
        connection_id,
        Some(connection_id),
        Some(session_id),
        MULTIPLEX_DATA_WS_SUBPROTOCOL,
    )
    .await?;
    let (sink, stream, initial_policy, accepted_session_id) = authenticate_split_socket(
        config,
        connection_id,
        Some(session_id),
        MultiplexConnectionRole::Data,
        opened,
    )
    .await?;
    Ok(AuthenticatedSplitDataConnection {
        sink,
        stream,
        initial_policy,
        session_id: accepted_session_id,
        connection_id: connection_id.to_string(),
    })
}

pub(super) async fn connect_authenticated_tunnel_impl(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    connection_id: &str,
) -> Result<AuthenticatedTunnelConnection, String> {
    let offered = format!("{MULTIPLEX_WS_SUBPROTOCOL}, {WS_SUBPROTOCOL}");
    let opened = open_socket(config, worker_id, Some(connection_id), None, &offered).await?;
    match opened.selected_protocol.as_str() {
        MULTIPLEX_WS_SUBPROTOCOL => authenticate_multiplex_socket(config, connection_id, opened)
            .await
            .map(AuthenticatedTunnelConnection::Multiplex),
        WS_SUBPROTOCOL => authenticate_legacy_socket(config, worker_id, opened)
            .await
            .map(AuthenticatedTunnelConnection::Legacy),
        selected => Err(format!(
            "server selected unsupported tunnel subprotocol {selected}"
        )),
    }
}

pub(super) async fn connect_authenticated_tunnel(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
    connection_id: &str,
) -> Result<AuthenticatedTunnelConnection, String> {
    connect_authenticated_tunnel_impl(config, worker_id, connection_id).await
}

fn verify_server_signature(
    key: &ed25519_dalek::VerifyingKey,
    payload: &[u8],
    encoded_signature: &str,
    context: &str,
) -> Result<(), String> {
    let signature = URL_SAFE_NO_PAD
        .decode(encoded_signature.as_bytes())
        .map_err(|_| format!("tunnel server {context} signature is invalid"))?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| format!("tunnel server {context} signature has invalid length"))?;
    key.verify(payload, &Signature::from_bytes(&signature))
        .map_err(|_| format!("tunnel server {context} signature is invalid"))
}

pub(super) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn rejects_server_signature_from_a_different_key() {
        let trusted = SigningKey::from_bytes(&[41_u8; 32]);
        let attacker = SigningKey::from_bytes(&[42_u8; 32]);
        let payload = b"signed-server-challenge";
        let forged = URL_SAFE_NO_PAD.encode(attacker.sign(payload).to_bytes());

        let error =
            verify_server_signature(&trusted.verifying_key(), payload, &forged, "challenge")
                .expect_err("a different server key must not be accepted");
        assert!(error.contains("signature is invalid"));
    }

    #[test]
    fn accepts_server_signature_from_the_pinned_key() {
        let trusted = SigningKey::from_bytes(&[43_u8; 32]);
        let payload = b"signed-server-ack";
        let signature = URL_SAFE_NO_PAD.encode(trusted.sign(payload).to_bytes());

        verify_server_signature(
            &trusted.verifying_key(),
            payload,
            &signature,
            "acknowledgement",
        )
        .expect("the pinned server key should verify");
    }
}
