use super::*;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use coding_tools_tunnel_protocol::{
    decode_multiplex_binary_frame, device_session_auth_signing_payload,
    multiplex_server_ack_signing_payload, multiplex_server_challenge_signing_payload,
    server_ack_signing_payload, server_challenge_signing_payload, split_server_ack_signing_payload,
    split_session_auth_signing_payload, ControlMessage, MultiplexBinaryFrameKind,
    MultiplexConnectionPolicy, MultiplexConnectionRole, MultiplexControlMessage,
    CONNECTION_ID_HEADER, DEVICE_ID_HEADER, MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
    MULTIPLEX_DATA_WS_SUBPROTOCOL, MULTIPLEX_PROTOCOL_VERSION, MULTIPLEX_WS_SUBPROTOCOL,
    PROTOCOL_VERSION, SESSION_ID_HEADER, WORKER_ID_HEADER, WS_PATH, WS_SUBPROTOCOL,
};
use ed25519_dalek::{Signer, VerifyingKey};
use std::sync::Mutex as StdMutex;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

use super::connection::unix_ms;
use super::identity::parse_enrollment_url;
use super::pool_policy::{
    multiplex_pool_adjustment, MultiplexPoolSnapshot, PoolAdjustment, ScaleUpBlock,
    ESTABLISHED_RECONNECT_DELAY, MAX_RECONNECT_DELAY,
};
use super::request_mapping::local_path_for_request;

fn single_worker_policy() -> WorkerPolicy {
    let mut policy = WorkerPolicy::default_for(TunnelService::Mcp);
    policy.start_workers = 1;
    policy.min_idle_workers = 1;
    policy.max_idle_workers = 1;
    policy.max_workers = 1;
    policy.max_connecting_workers = 1;
    policy.scale_down_step = 1;
    policy.burst_warm_workers = 1;
    policy
}

const TEST_SERVER_ID: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

fn test_server_signing_key() -> SigningKey {
    SigningKey::from_bytes(&[91_u8; 32])
}

fn test_server_verifying_key() -> Arc<VerifyingKey> {
    Arc::new(test_server_signing_key().verifying_key())
}

async fn accept_authenticated_test_worker(
    stream: tokio::net::TcpStream,
    nonce: String,
    policy: WorkerPolicy,
    expected_protocols: &[&str],
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    let expected_protocols = expected_protocols.to_vec();
    let connection_identity = Arc::new(StdMutex::new(None::<(String, String)>));
    let captured = connection_identity.clone();
    let mut socket = tokio_tungstenite::accept_hdr_async(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let offered = request
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok())
                .expect("offered subprotocols");
            for protocol in &expected_protocols {
                assert!(
                    offered.split(',').any(|item| item.trim() == *protocol),
                    "client did not offer expected tunnel subprotocol {protocol}: {offered}"
                );
            }
            let device_id = request
                .headers()
                .get(DEVICE_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("device id header")
                .to_string();
            let worker_id = request
                .headers()
                .get(WORKER_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("worker id header")
                .to_string();
            *captured.lock().expect("capture connection identity") = Some((device_id, worker_id));
            response.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                WS_SUBPROTOCOL.parse().expect("subprotocol header"),
            );
            Ok(response)
        },
    )
    .await
    .expect("accept websocket");
    let (device_id, worker_id) = connection_identity
        .lock()
        .expect("connection identity")
        .clone()
        .expect("captured connection identity");
    let expires_at_unix_ms = unix_ms().saturating_add(10_000);
    let server_key = test_server_signing_key();
    let challenge_signature = URL_SAFE_NO_PAD.encode(
        server_key
            .sign(&server_challenge_signing_payload(
                &nonce,
                expires_at_unix_ms,
                TEST_SERVER_ID,
                &device_id,
                "pc-a",
                TunnelService::Mcp,
                &worker_id,
            ))
            .to_bytes(),
    );
    socket
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Challenge {
                nonce: nonce.clone(),
                expires_at_unix_ms,
                server_id: TEST_SERVER_ID.into(),
                server_signature: challenge_signature,
            })
            .expect("challenge json")
            .into(),
        ))
        .await
        .expect("send challenge");
    let authenticate = socket
        .next()
        .await
        .expect("authenticate frame")
        .expect("authenticate frame");
    let Message::Text(authenticate) = authenticate else {
        panic!("expected authenticate text");
    };
    let ControlMessage::Authenticate(proof) =
        serde_json::from_str::<ControlMessage>(authenticate.as_ref()).expect("authenticate json")
    else {
        panic!("expected authenticate control");
    };
    let ack_signature = URL_SAFE_NO_PAD.encode(
        server_key
            .sign(&server_ack_signing_payload(
                &nonce,
                TEST_SERVER_ID,
                &proof,
                &policy,
            ))
            .to_bytes(),
    );
    socket
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                worker_policy: policy,
                server_id: TEST_SERVER_ID.into(),
                server_signature: ack_signature,
            })
            .expect("hello ack json")
            .into(),
        ))
        .await
        .expect("send hello ack");
    socket
}

#[tokio::test]
async fn negotiates_v5_device_session_and_registers_route_with_v4_fallback_offered() {
    use super::connection::{connect_authenticated_tunnel, AuthenticatedTunnelConnection};
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let device_key = SigningKey::from_bytes(&[33_u8; 32]);
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: "http://127.0.0.1:1".into(),
        device_id: "device-v5".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-connection-test.log"),
    };

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let captured = Arc::new(StdMutex::new(None::<(String, String)>));
        let captured_cb = captured.clone();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                  mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                let offered = request
                    .headers()
                    .get(SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|value| value.to_str().ok())
                    .expect("offered subprotocols");
                assert!(offered.contains(MULTIPLEX_WS_SUBPROTOCOL));
                assert!(offered.contains(WS_SUBPROTOCOL));
                let device_id = request
                    .headers()
                    .get(DEVICE_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .expect("device id")
                    .to_string();
                let connection_id = request
                    .headers()
                    .get(CONNECTION_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .expect("connection id")
                    .to_string();
                *captured_cb.lock().expect("capture") = Some((device_id, connection_id));
                response.headers_mut().insert(
                    SEC_WEBSOCKET_PROTOCOL,
                    MULTIPLEX_WS_SUBPROTOCOL.parse().expect("v5 protocol"),
                );
                Ok(response)
            },
        )
        .await
        .expect("accept websocket");
        let (device_id, connection_id) = captured
            .lock()
            .expect("captured")
            .clone()
            .expect("identity");
        let nonce = "rust-v5-nonce".to_string();
        let expires_at_unix_ms = unix_ms().saturating_add(10_000);
        let server_key = test_server_signing_key();
        let server_signature = URL_SAFE_NO_PAD.encode(
            server_key
                .sign(&multiplex_server_challenge_signing_payload(
                    &nonce,
                    expires_at_unix_ms,
                    TEST_SERVER_ID,
                    &device_id,
                    &connection_id,
                ))
                .to_bytes(),
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::Challenge {
                    nonce: nonce.clone(),
                    expires_at_unix_ms,
                    server_id: TEST_SERVER_ID.into(),
                    server_signature,
                })
                .expect("challenge")
                .into(),
            ))
            .await
            .expect("send challenge");
        let authenticate = socket.next().await.expect("auth frame").expect("auth");
        let Message::Text(authenticate) = authenticate else {
            panic!("expected auth text")
        };
        let MultiplexControlMessage::Authenticate(proof) =
            serde_json::from_str::<MultiplexControlMessage>(authenticate.as_ref())
                .expect("auth json")
        else {
            panic!("expected v5 authenticate")
        };
        assert_eq!(proof.device_id, device_id);
        assert_eq!(proof.hello.connection_id, connection_id);
        device_key
            .verifying_key()
            .verify_strict(
                &device_session_auth_signing_payload(&nonce, &proof),
                &ed25519_dalek::Signature::from_slice(
                    &URL_SAFE_NO_PAD
                        .decode(proof.signature.as_bytes())
                        .expect("signature bytes"),
                )
                .expect("signature"),
            )
            .expect("device session signature");
        let policy = MultiplexConnectionPolicy::default();
        let ack_signature = URL_SAFE_NO_PAD.encode(
            server_key
                .sign(&multiplex_server_ack_signing_payload(
                    &nonce,
                    TEST_SERVER_ID,
                    &proof,
                    &policy,
                ))
                .to_bytes(),
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::HelloAck {
                    protocol_version: MULTIPLEX_PROTOCOL_VERSION,
                    connection_policy: policy,
                    server_id: TEST_SERVER_ID.into(),
                    server_signature: ack_signature,
                })
                .expect("ack")
                .into(),
            ))
            .await
            .expect("send ack");
        let registration = socket
            .next()
            .await
            .expect("register frame")
            .expect("register");
        let Message::Text(registration) = registration else {
            panic!("expected registration text")
        };
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            service,
        } = serde_json::from_str::<MultiplexControlMessage>(registration.as_ref())
            .expect("registration json")
        else {
            panic!("expected route register")
        };
        assert_eq!(client_id, "pc-a");
        assert_eq!(service, TunnelService::Mcp);
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 9,
                    stream_limit: 16,
                })
                .expect("route ack")
                .into(),
            ))
            .await
            .expect("send route ack");
    });

    let connection = connect_authenticated_tunnel(&config, "worker-v4-fallback", "connection-v5")
        .await
        .expect("connect v5");
    match connection {
        AuthenticatedTunnelConnection::Multiplex(connection) => {
            assert_eq!(connection.route_id, 9);
            assert_eq!(connection.stream_limit, 16);
            assert_eq!(
                connection.initial_policy,
                MultiplexConnectionPolicy::default()
            );
        }
        AuthenticatedTunnelConnection::Legacy(_) => panic!("server selected v5"),
    }
    server.await.expect("server task");
}

#[tokio::test]
async fn split_client_falls_back_to_v4_only_server() {
    use super::connection::AuthenticatedTunnelConnection;
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("listener address");
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: "http://127.0.0.1:1".into(),
        device_id: "device-v4-fallback".into(),
        signing_key: Arc::new(SigningKey::from_bytes(&[34_u8; 32])),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v4-fallback-test.log"),
    };
    let policy = single_worker_policy();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        let _socket = accept_authenticated_test_worker(
            stream,
            "rust-v4-fallback-nonce".into(),
            policy,
            &[
                MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
                MULTIPLEX_WS_SUBPROTOCOL,
                WS_SUBPROTOCOL,
            ],
        )
        .await;
    });

    let negotiation = connect_split_control_or_fallback(&config, "rust-control-v4-fallback")
        .await
        .expect("v4 fallback negotiation");
    assert!(matches!(
        negotiation,
        SplitControlNegotiation::Fallback(AuthenticatedTunnelConnection::Legacy(_))
    ));
    server.await.expect("v4-only server task");
}

#[tokio::test]
async fn split_control_negotiates_session_and_route_on_the_initial_socket() {
    use tokio::net::TcpListener;

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
    let address = listener.local_addr().expect("address");
    let device_key = SigningKey::from_bytes(&[36_u8; 32]);
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: "http://127.0.0.1:1".into(),
        device_id: "device-split-control".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-control-test.log"),
    };

    let server = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await.expect("accept");
        let captured = Arc::new(StdMutex::new(None::<(String, String)>));
        let captured_headers = captured.clone();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            tcp,
            move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                  mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                let offered = request
                    .headers()
                    .get(SEC_WEBSOCKET_PROTOCOL)
                    .and_then(|value| value.to_str().ok())
                    .expect("protocols")
                    .to_string();
                assert!(offered.contains(MULTIPLEX_CONTROL_WS_SUBPROTOCOL));
                assert!(offered.contains(MULTIPLEX_WS_SUBPROTOCOL));
                assert!(offered.contains(WS_SUBPROTOCOL));
                let device_id = request
                    .headers()
                    .get(DEVICE_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .expect("device id")
                    .to_string();
                let connection_id = request
                    .headers()
                    .get(CONNECTION_ID_HEADER)
                    .and_then(|value| value.to_str().ok())
                    .expect("connection id")
                    .to_string();
                *captured_headers.lock().expect("headers") = Some((device_id, connection_id));
                response.headers_mut().insert(
                    SEC_WEBSOCKET_PROTOCOL,
                    MULTIPLEX_CONTROL_WS_SUBPROTOCOL.parse().unwrap(),
                );
                Ok(response)
            },
        )
        .await
        .expect("websocket");
        let (device_id, connection_id) = captured.lock().unwrap().clone().unwrap();
        let nonce = "split-control-nonce".to_string();
        let expires_at_unix_ms = unix_ms().saturating_add(10_000);
        let server_key = test_server_signing_key();
        let challenge_signature = URL_SAFE_NO_PAD.encode(
            server_key
                .sign(&multiplex_server_challenge_signing_payload(
                    &nonce,
                    expires_at_unix_ms,
                    TEST_SERVER_ID,
                    &device_id,
                    &connection_id,
                ))
                .to_bytes(),
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::Challenge {
                    nonce: nonce.clone(),
                    expires_at_unix_ms,
                    server_id: TEST_SERVER_ID.into(),
                    server_signature: challenge_signature,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let auth = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::SessionAuthenticate(proof) =
            serde_json::from_str::<MultiplexControlMessage>(&auth).unwrap()
        else {
            panic!("split session authenticate")
        };
        assert_eq!(proof.device_id, device_id);
        assert_eq!(
            proof.hello.connection_role,
            MultiplexConnectionRole::Control
        );
        assert_eq!(proof.hello.connection_id, connection_id);
        assert_eq!(proof.hello.session_id, None);
        device_key
            .verifying_key()
            .verify_strict(
                &split_session_auth_signing_payload(&nonce, &proof),
                &ed25519_dalek::Signature::from_slice(
                    &URL_SAFE_NO_PAD.decode(proof.signature.as_bytes()).unwrap(),
                )
                .unwrap(),
            )
            .expect("split proof signature");

        let policy = MultiplexConnectionPolicy::default();
        let session_id = "session-rust-split".to_string();
        let ack_signature = URL_SAFE_NO_PAD.encode(
            server_key
                .sign(&split_server_ack_signing_payload(
                    &nonce,
                    TEST_SERVER_ID,
                    &proof,
                    &session_id,
                    &policy,
                ))
                .to_bytes(),
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::SessionReady {
                    protocol_version: MULTIPLEX_PROTOCOL_VERSION,
                    connection_role: MultiplexConnectionRole::Control,
                    session_id: session_id.clone(),
                    connection_policy: policy,
                    server_id: TEST_SERVER_ID.into(),
                    server_signature: ack_signature,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let register = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            service,
        } = serde_json::from_str::<MultiplexControlMessage>(&register).unwrap()
        else {
            panic!("route register")
        };
        assert_eq!(client_id, "pc-a");
        assert_eq!(service, TunnelService::Mcp);
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 41,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        socket.close(None).await.unwrap();
    });

    let negotiation = connect_split_control_or_fallback(&config, "rust-control-1")
        .await
        .expect("split negotiation");
    let SplitControlNegotiation::Split(connection) = negotiation else {
        panic!("server selected split control")
    };
    assert_eq!(connection.session_id, "session-rust-split");
    assert_eq!(connection.route_id, 41);
    assert_eq!(connection.stream_limit, 16);
    assert_eq!(
        connection.initial_policy,
        MultiplexConnectionPolicy::default()
    );
    drop(connection);
    server.await.expect("server");
}

async fn accept_authenticated_split_test_connection(
    stream: tokio::net::TcpStream,
    role: MultiplexConnectionRole,
    device_key: &SigningKey,
    session_id: &str,
    policy: MultiplexConnectionPolicy,
) -> tokio_tungstenite::WebSocketStream<tokio::net::TcpStream> {
    let captured = Arc::new(StdMutex::new(None::<(String, String, Option<String>)>));
    let captured_headers = captured.clone();
    let expected_protocol = match role {
        MultiplexConnectionRole::Control => MULTIPLEX_CONTROL_WS_SUBPROTOCOL,
        MultiplexConnectionRole::Data => MULTIPLEX_DATA_WS_SUBPROTOCOL,
    };
    let mut socket = tokio_tungstenite::accept_hdr_async(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let offered = request
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok())
                .expect("split protocols");
            assert!(offered.contains(expected_protocol));
            let device_id = request
                .headers()
                .get(DEVICE_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("split device id")
                .to_string();
            let connection_id = request
                .headers()
                .get(CONNECTION_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("split connection id")
                .to_string();
            let attached_session = request
                .headers()
                .get(SESSION_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            *captured_headers.lock().expect("split headers") =
                Some((device_id, connection_id, attached_session));
            response.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                expected_protocol.parse().expect("split protocol"),
            );
            Ok(response)
        },
    )
    .await
    .expect("accept split websocket");
    let (device_id, connection_id, attached_session) = captured
        .lock()
        .unwrap()
        .clone()
        .expect("captured split headers");
    match role {
        MultiplexConnectionRole::Control => assert_eq!(attached_session, None),
        MultiplexConnectionRole::Data => assert_eq!(attached_session.as_deref(), Some(session_id)),
    }
    let nonce = format!("split-{role:?}-{connection_id}");
    let expires_at_unix_ms = unix_ms().saturating_add(10_000);
    let server_key = test_server_signing_key();
    let challenge_signature = URL_SAFE_NO_PAD.encode(
        server_key
            .sign(&multiplex_server_challenge_signing_payload(
                &nonce,
                expires_at_unix_ms,
                TEST_SERVER_ID,
                &device_id,
                &connection_id,
            ))
            .to_bytes(),
    );
    socket
        .send(Message::Text(
            serde_json::to_string(&MultiplexControlMessage::Challenge {
                nonce: nonce.clone(),
                expires_at_unix_ms,
                server_id: TEST_SERVER_ID.into(),
                server_signature: challenge_signature,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    let auth = socket.next().await.unwrap().unwrap().into_text().unwrap();
    let MultiplexControlMessage::SessionAuthenticate(proof) =
        serde_json::from_str::<MultiplexControlMessage>(&auth).unwrap()
    else {
        panic!("expected split authenticate")
    };
    assert_eq!(proof.device_id, device_id);
    assert_eq!(proof.hello.connection_role, role);
    assert_eq!(proof.hello.connection_id, connection_id);
    match role {
        MultiplexConnectionRole::Control => assert_eq!(proof.hello.session_id, None),
        MultiplexConnectionRole::Data => {
            assert_eq!(proof.hello.session_id.as_deref(), Some(session_id))
        }
    }
    device_key
        .verifying_key()
        .verify_strict(
            &split_session_auth_signing_payload(&nonce, &proof),
            &ed25519_dalek::Signature::from_slice(
                &URL_SAFE_NO_PAD.decode(proof.signature.as_bytes()).unwrap(),
            )
            .unwrap(),
        )
        .expect("split device signature");
    let ack_signature = URL_SAFE_NO_PAD.encode(
        server_key
            .sign(&split_server_ack_signing_payload(
                &nonce,
                TEST_SERVER_ID,
                &proof,
                session_id,
                &policy,
            ))
            .to_bytes(),
    );
    socket
        .send(Message::Text(
            serde_json::to_string(&MultiplexControlMessage::SessionReady {
                protocol_version: MULTIPLEX_PROTOCOL_VERSION,
                connection_role: role,
                session_id: session_id.to_string(),
                connection_policy: policy,
                server_id: TEST_SERVER_ID.into(),
                server_signature: ack_signature,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
    socket
}

async fn reject_split_control_rebind_test_connection(
    stream: tokio::net::TcpStream,
    expected_session_id: &str,
) {
    let captured_session = Arc::new(StdMutex::new(None::<String>));
    let captured = captured_session.clone();
    let mut socket = tokio_tungstenite::accept_hdr_async(
        stream,
        move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
              mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let offered = request
                .headers()
                .get(SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok())
                .expect("split reconnect protocols");
            assert!(offered.contains(MULTIPLEX_CONTROL_WS_SUBPROTOCOL));
            let session_id = request
                .headers()
                .get(SESSION_ID_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("split reconnect session id")
                .to_string();
            *captured.lock().expect("split reconnect header") = Some(session_id);
            response.headers_mut().insert(
                SEC_WEBSOCKET_PROTOCOL,
                MULTIPLEX_CONTROL_WS_SUBPROTOCOL
                    .parse()
                    .expect("split control protocol"),
            );
            Ok(response)
        },
    )
    .await
    .expect("accept split reconnect websocket");
    assert_eq!(
        captured_session.lock().unwrap().as_deref(),
        Some(expected_session_id)
    );
    socket.close(None).await.expect("close rejected reconnect");
}

#[tokio::test]
async fn split_worker_pool_replays_add_interrupted_before_route_ack() {
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let tunnel_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("tunnel listener");
    let tunnel_address = tunnel_listener.local_addr().expect("tunnel address");
    let device_key = SigningKey::from_bytes(&[40_u8; 32]);
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-owner/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-owner".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-owner".into(),
        local_base_url: "http://127.0.0.1:1".into(),
        device_id: "device-split-interrupted-add".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-interrupted-add.log"),
    };
    let second_config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-second/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-second".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-second".into(),
        local_base_url: "http://127.0.0.1:2".into(),
        device_id: "device-split-interrupted-add".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-interrupted-add-second.log"),
    };
    let policy = MultiplexConnectionPolicy::default();
    let old_session = "session-rust-interrupted-add".to_string();
    let new_session = "session-rust-interrupted-add-replacement".to_string();
    let server_device_key = device_key.clone();
    let server_policy = policy.clone();
    let server_old_session = old_session.clone();
    let server_new_session = new_session.clone();
    let (done_tx, done_rx) = oneshot::channel::<()>();

    let server = tokio::spawn(async move {
        let (control_tcp, _) = tunnel_listener
            .accept()
            .await
            .expect("initial control accept");
        let mut control = accept_authenticated_split_test_connection(
            control_tcp,
            MultiplexConnectionRole::Control,
            &server_device_key,
            &server_old_session,
            server_policy.clone(),
        )
        .await;
        let initial = control.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            ..
        } = serde_json::from_str::<MultiplexControlMessage>(&initial).unwrap()
        else {
            panic!("initial route register")
        };
        assert_eq!(client_id, "pc-owner");
        control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 51,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let (data_tcp, _) = tunnel_listener.accept().await.expect("initial data accept");
        let data = accept_authenticated_split_test_connection(
            data_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_old_session,
            server_policy.clone(),
        )
        .await;

        let interrupted = control.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::RouteRegister { client_id, .. } =
            serde_json::from_str::<MultiplexControlMessage>(&interrupted).unwrap()
        else {
            panic!("interrupted shared route register")
        };
        assert_eq!(client_id, "pc-second");
        control
            .close(None)
            .await
            .expect("interrupt shared route ack");

        for _ in 0..3 {
            let (rebind_tcp, _) =
                tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
                    .await
                    .expect("old-session rebind timeout")
                    .expect("old-session rebind accept");
            reject_split_control_rebind_test_connection(rebind_tcp, &server_old_session).await;
        }

        let (replacement_control_tcp, _) =
            tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
                .await
                .expect("replacement control timeout")
                .expect("replacement control accept");
        let mut replacement_control = accept_authenticated_split_test_connection(
            replacement_control_tcp,
            MultiplexConnectionRole::Control,
            &server_device_key,
            &server_new_session,
            server_policy.clone(),
        )
        .await;

        let owner = replacement_control
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            ..
        } = serde_json::from_str::<MultiplexControlMessage>(&owner).unwrap()
        else {
            panic!("replacement owner route register")
        };
        assert_eq!(client_id, "pc-owner");
        replacement_control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 61,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let replay = replacement_control
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            ..
        } = serde_json::from_str::<MultiplexControlMessage>(&replay).unwrap()
        else {
            panic!("replacement replay route register")
        };
        assert_eq!(client_id, "pc-second");
        replacement_control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 62,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let (replacement_data_tcp, _) =
            tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
                .await
                .expect("replacement data timeout")
                .expect("replacement data accept");
        let replacement_data = accept_authenticated_split_test_connection(
            replacement_data_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_new_session,
            server_policy,
        )
        .await;
        let _ = done_tx.send(());
        drop((data, replacement_control, replacement_data));
    });

    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, _status_rx) = mpsc::channel(16);
    let (route_tx, route_rx) = mpsc::channel(8);
    let pool = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics,
        route_rx,
    ));

    let (first_tx, first_rx) = oneshot::channel();
    route_tx
        .send(SplitRouteCommand::Add {
            config: second_config.clone(),
            response: first_tx,
        })
        .await
        .expect("send interrupted shared route");
    assert!(
        tokio::time::timeout(Duration::from_secs(5), first_rx)
            .await
            .expect("interrupted response timeout")
            .is_err(),
        "session failure should close the in-flight response so the handle can retry"
    );

    let (retry_tx, retry_rx) = oneshot::channel();
    route_tx
        .send(SplitRouteCommand::Add {
            config: second_config,
            response: retry_tx,
        })
        .await
        .expect("retry interrupted shared route");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), retry_rx)
            .await
            .expect("retried response timeout")
            .expect("retried response")
            .expect("retried route result"),
        (62, 16)
    );
    tokio::time::timeout(Duration::from_secs(10), done_rx)
        .await
        .expect("replacement server timeout")
        .expect("replacement server completion");

    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("interrupted add pool shutdown timeout")
        .expect("interrupted add pool task");
    server.await.expect("interrupted add server");
}

#[tokio::test]
async fn split_worker_pool_rebuilds_after_control_session_grace_expires() {
    use axum::extract::RawQuery;
    use axum::routing::any;
    use axum::Router;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let local_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let local_address = local_listener.local_addr().expect("local address");
    let local_server = tokio::spawn(async move {
        let app = Router::new().fallback(any(|RawQuery(query): RawQuery| async move {
            format!("owner:{}", query.unwrap_or_else(|| "none".into()))
        }));
        axum::serve(local_listener, app)
            .await
            .expect("local server");
    });

    let survivor_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("survivor local listener");
    let survivor_address = survivor_listener
        .local_addr()
        .expect("survivor local address");
    let survivor_server = tokio::spawn(async move {
        let app = Router::new().fallback(any(|RawQuery(query): RawQuery| async move {
            format!("survivor:{}", query.unwrap_or_else(|| "none".into()))
        }));
        axum::serve(survivor_listener, app)
            .await
            .expect("survivor local server");
    });

    let tunnel_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("tunnel listener");
    let tunnel_address = tunnel_listener.local_addr().expect("tunnel address");
    let device_key = SigningKey::from_bytes(&[39_u8; 32]);
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-expired/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-expired".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-expired".into(),
        local_base_url: format!("http://{local_address}"),
        device_id: "device-split-expired".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-expired-test.log"),
    };
    let survivor_config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-survivor/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-survivor".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-survivor".into(),
        local_base_url: format!("http://{survivor_address}"),
        device_id: "device-split-expired".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-survivor-test.log"),
    };
    let policy = MultiplexConnectionPolicy::default();
    let old_session = "session-rust-expired".to_string();
    let new_session = "session-rust-replacement".to_string();
    let server_device_key = device_key.clone();
    let server_policy = policy.clone();
    let server_old_session = old_session.clone();
    let server_new_session = new_session.clone();
    let (done_tx, done_rx) = oneshot::channel::<()>();

    let server = tokio::spawn(async move {
        let (control_tcp, _) = tunnel_listener
            .accept()
            .await
            .expect("initial control accept");
        let mut control = accept_authenticated_split_test_connection(
            control_tcp,
            MultiplexConnectionRole::Control,
            &server_device_key,
            &server_old_session,
            server_policy.clone(),
        )
        .await;
        let registration = control.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            service,
        } = serde_json::from_str::<MultiplexControlMessage>(&registration).unwrap()
        else {
            panic!("initial route register")
        };
        assert_eq!(client_id, "pc-expired");
        assert_eq!(service, TunnelService::Mcp);
        control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 51,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let (data_tcp, _) = tunnel_listener.accept().await.expect("initial data accept");
        let data = accept_authenticated_split_test_connection(
            data_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_old_session,
            server_policy.clone(),
        )
        .await;

        let second_registration = loop {
            match control.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    let message = serde_json::from_str::<MultiplexControlMessage>(&text).unwrap();
                    match message {
                        MultiplexControlMessage::RouteRegister {
                            registration_id,
                            client_id,
                            service,
                        } => break (registration_id, client_id, service),
                        other => {
                            panic!("unexpected control before survivor registration: {other:?}")
                        }
                    }
                }
                Message::Ping(payload) => control.send(Message::Pong(payload)).await.unwrap(),
                Message::Pong(_) => {}
                other => panic!("unexpected frame before survivor registration: {other:?}"),
            }
        };
        assert_eq!(second_registration.1, "pc-survivor");
        assert_eq!(second_registration.2, TunnelService::Mcp);
        control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id: second_registration.0,
                    route_id: 52,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        loop {
            match control.next().await.unwrap().unwrap() {
                Message::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(&text).unwrap() {
                        MultiplexControlMessage::RouteUnregister { route_id: 51 } => break,
                        other => panic!("unexpected control before owner removal: {other:?}"),
                    }
                }
                Message::Ping(payload) => control.send(Message::Pong(payload)).await.unwrap(),
                Message::Pong(_) => {}
                other => panic!("unexpected frame before owner removal: {other:?}"),
            }
        }
        control.close(None).await.expect("drop initial control");

        for _ in 0..3 {
            let (rebind_tcp, _) =
                tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
                    .await
                    .expect("old-session rebind timeout")
                    .expect("old-session rebind accept");
            reject_split_control_rebind_test_connection(rebind_tcp, &server_old_session).await;
        }

        let (replacement_control_tcp, _) =
            tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
                .await
                .expect("replacement control timeout")
                .expect("replacement control accept");
        let mut replacement_control = accept_authenticated_split_test_connection(
            replacement_control_tcp,
            MultiplexConnectionRole::Control,
            &server_device_key,
            &server_new_session,
            server_policy.clone(),
        )
        .await;
        let registration = replacement_control
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id,
            client_id,
            service,
        } = serde_json::from_str::<MultiplexControlMessage>(&registration).unwrap()
        else {
            panic!("replacement route register")
        };
        assert_eq!(client_id, "pc-survivor");
        assert_eq!(service, TunnelService::Mcp);
        replacement_control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 61,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let (replacement_data_tcp, _) =
            tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
                .await
                .expect("replacement data timeout")
                .expect("replacement data accept");
        let mut replacement_data = accept_authenticated_split_test_connection(
            replacement_data_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_new_session,
            server_policy,
        )
        .await;
        replacement_data
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RequestHead {
                    route_id: 61,
                    stream_id: 201,
                    method: "GET".into(),
                    path_and_query: "/builtin/clients/pc-survivor/mcp?after=expiry".into(),
                    headers: vec![],
                    demand: None,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        replacement_data
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RequestEnd {
                    route_id: 61,
                    stream_id: 201,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let mut body = Vec::new();
        let mut ended = false;
        while !ended {
            match tokio::time::timeout(Duration::from_secs(5), replacement_data.next())
                .await
                .expect("replacement response timeout")
                .expect("replacement response frame")
                .expect("replacement response")
            {
                Message::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(&text).unwrap() {
                        MultiplexControlMessage::ResponseHead {
                            route_id: 61,
                            stream_id: 201,
                            status: 200,
                            ..
                        } => {}
                        MultiplexControlMessage::ResponseEnd {
                            route_id: 61,
                            stream_id: 201,
                        } => ended = true,
                        MultiplexControlMessage::Error { message, .. } => {
                            panic!("replacement route error: {message}")
                        }
                        other => panic!("unexpected replacement control: {other:?}"),
                    }
                }
                Message::Binary(frame) => {
                    let frame = decode_multiplex_binary_frame(frame.as_ref()).unwrap();
                    assert_eq!(frame.route_id, 61);
                    assert_eq!(frame.stream_id, 201);
                    assert_eq!(frame.kind, MultiplexBinaryFrameKind::ResponseBody);
                    body.extend_from_slice(&frame.payload);
                }
                Message::Ping(payload) => {
                    replacement_data.send(Message::Pong(payload)).await.unwrap()
                }
                other => panic!("unexpected replacement response frame: {other:?}"),
            }
        }
        assert_eq!(String::from_utf8(body).unwrap(), "survivor:after=expiry");
        drop((data, replacement_control, replacement_data));
        let _ = done_tx.send(());
    });

    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, mut status_rx) = mpsc::channel(16);
    let (route_tx, route_rx) = mpsc::channel(8);
    let pool = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
        route_rx,
    ));

    let (add_tx, add_rx) = oneshot::channel();
    route_tx
        .send(SplitRouteCommand::Add {
            config: survivor_config,
            response: add_tx,
        })
        .await
        .expect("send survivor route");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), add_rx)
            .await
            .expect("survivor add timeout")
            .expect("survivor add response")
            .expect("survivor add result"),
        (52, 16)
    );

    let (remove_tx, remove_rx) = oneshot::channel();
    route_tx
        .send(SplitRouteCommand::Remove {
            client_id: "pc-expired".into(),
            service: TunnelService::Mcp,
            response: remove_tx,
        })
        .await
        .expect("remove original owner route");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), remove_rx)
            .await
            .expect("owner removal timeout")
            .expect("owner removal response")
            .expect("owner removal result"),
        1
    );

    tokio::time::timeout(Duration::from_secs(10), done_rx)
        .await
        .expect("split session replacement timeout")
        .expect("split session replacement server finished");
    assert!(status_rx.try_recv().is_ok());
    let _ = shutdown_tx.send(true);
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("replacement pool shutdown timeout")
        .expect("replacement pool task");
    server.await.expect("replacement server");
    local_server.abort();
    let _ = local_server.await;
    survivor_server.abort();
    let _ = survivor_server.await;
}

#[tokio::test]
async fn split_worker_pool_shares_routes_and_scales_data_without_scaling_control() {
    use axum::extract::RawQuery;
    use axum::routing::any;
    use axum::Router;
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    let local_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let local_address = local_listener.local_addr().expect("local address");
    let local_server = tokio::spawn(async move {
        let app = Router::new().fallback(any(|RawQuery(query): RawQuery| async move {
            query.unwrap_or_else(|| "none".into())
        }));
        axum::serve(local_listener, app)
            .await
            .expect("local server");
    });

    let second_local_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("second local listener");
    let second_local_address = second_local_listener
        .local_addr()
        .expect("second local address");
    let second_local_server = tokio::spawn(async move {
        let app = Router::new().fallback(any(|RawQuery(query): RawQuery| async move {
            format!("second:{}", query.unwrap_or_else(|| "none".into()))
        }));
        axum::serve(second_local_listener, app)
            .await
            .expect("second local server");
    });

    let tunnel_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("tunnel listener");
    let tunnel_address = tunnel_listener.local_addr().expect("tunnel address");
    let device_key = SigningKey::from_bytes(&[37_u8; 32]);
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: format!("http://{local_address}"),
        device_id: "device-split-scale".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-scale-test.log"),
    };
    let second_config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-b/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-b".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-b".into(),
        local_base_url: format!("http://{second_local_address}"),
        device_id: "device-split-scale".into(),
        signing_key: Arc::new(device_key.clone()),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-split-scale-b-test.log"),
    };
    let policy = MultiplexConnectionPolicy::default();
    let session_id = "session-rust-scale".to_string();
    let (done_tx, done_rx) = oneshot::channel::<()>();
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let server_device_key = device_key.clone();
    let server_policy = policy.clone();
    let server_session = session_id.clone();
    let server = tokio::spawn(async move {
        let (control_tcp, _) = tunnel_listener.accept().await.expect("control accept");
        let mut control = accept_authenticated_split_test_connection(
            control_tcp,
            MultiplexConnectionRole::Control,
            &server_device_key,
            &server_session,
            server_policy.clone(),
        )
        .await;
        let registration = control.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id, ..
        } = serde_json::from_str::<MultiplexControlMessage>(&registration).unwrap()
        else {
            panic!("route register")
        };
        control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 51,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let (data1_tcp, _) = tunnel_listener.accept().await.expect("data 1 accept");
        let mut data1 = accept_authenticated_split_test_connection(
            data1_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_session,
            server_policy.clone(),
        )
        .await;
        let (second_registration_id, client_id, service) = loop {
            match control.next().await.unwrap().unwrap() {
                Message::Text(second_registration) => {
                    let MultiplexControlMessage::RouteRegister {
                        registration_id,
                        client_id,
                        service,
                    } = serde_json::from_str::<MultiplexControlMessage>(&second_registration)
                        .unwrap()
                    else {
                        panic!("second route register")
                    };
                    break (registration_id, client_id, service);
                }
                Message::Ping(payload) => control.send(Message::Pong(payload)).await.unwrap(),
                Message::Pong(_) => {}
                other => panic!("unexpected control frame before second route: {other:?}"),
            }
        };
        assert_eq!(client_id, "pc-b");
        assert_eq!(service, TunnelService::Mcp);
        control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id: second_registration_id,
                    route_id: 52,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        control
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::CapacityUpdate {
                    demand: MultiplexDemand {
                        queued_requests: 0,
                        oldest_queue_wait_ms: 0,
                        active_streams: 12,
                        available_stream_slots: 4,
                        desired_connections: 1,
                    },
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        let (data2_tcp, _) = tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
            .await
            .expect("data 2 scale timeout")
            .expect("data 2 accept");
        let data2 = accept_authenticated_split_test_connection(
            data2_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_session,
            server_policy.clone(),
        )
        .await;
        let (data3_tcp, _) = tokio::time::timeout(Duration::from_secs(5), tunnel_listener.accept())
            .await
            .expect("data 3 scale timeout")
            .expect("data 3 accept");
        let data3 = accept_authenticated_split_test_connection(
            data3_tcp,
            MultiplexConnectionRole::Data,
            &server_device_key,
            &server_session,
            server_policy,
        )
        .await;

        data1
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RequestHead {
                    route_id: 51,
                    stream_id: 101,
                    method: "GET".into(),
                    path_and_query: "/builtin/clients/pc-a/mcp?split=ok".into(),
                    headers: vec![],
                    demand: None,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        data1
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RequestEnd {
                    route_id: 51,
                    stream_id: 101,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let mut body = Vec::new();
        let mut ended = false;
        while !ended {
            match tokio::time::timeout(Duration::from_secs(5), data1.next())
                .await
                .expect("split response timeout")
                .expect("split response frame")
                .expect("split response")
            {
                Message::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(&text).unwrap() {
                        MultiplexControlMessage::ResponseHead {
                            route_id: 51,
                            stream_id: 101,
                            status: 200,
                            ..
                        } => {}
                        MultiplexControlMessage::ResponseEnd {
                            route_id: 51,
                            stream_id: 101,
                        } => ended = true,
                        MultiplexControlMessage::Error { message, .. } => {
                            panic!("split data error: {message}")
                        }
                        other => panic!("unexpected split response control: {other:?}"),
                    }
                }
                Message::Binary(frame) => {
                    let frame = decode_multiplex_binary_frame(frame.as_ref()).unwrap();
                    assert_eq!(frame.route_id, 51);
                    assert_eq!(frame.stream_id, 101);
                    assert_eq!(frame.kind, MultiplexBinaryFrameKind::ResponseBody);
                    body.extend_from_slice(&frame.payload);
                }
                Message::Ping(payload) => data1.send(Message::Pong(payload)).await.unwrap(),
                other => panic!("unexpected split response frame: {other:?}"),
            }
        }
        assert_eq!(String::from_utf8(body).unwrap(), "split=ok");

        data1
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RequestHead {
                    route_id: 52,
                    stream_id: 102,
                    method: "GET".into(),
                    path_and_query: "/builtin/clients/pc-b/mcp?shared=ok".into(),
                    headers: vec![],
                    demand: None,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        data1
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RequestEnd {
                    route_id: 52,
                    stream_id: 102,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let mut second_body = Vec::new();
        let mut second_ended = false;
        while !second_ended {
            match tokio::time::timeout(Duration::from_secs(5), data1.next())
                .await
                .expect("shared route response timeout")
                .expect("shared route response frame")
                .expect("shared route response")
            {
                Message::Text(text) => {
                    match serde_json::from_str::<MultiplexControlMessage>(&text).unwrap() {
                        MultiplexControlMessage::ResponseHead {
                            route_id: 52,
                            stream_id: 102,
                            status: 200,
                            ..
                        } => {}
                        MultiplexControlMessage::ResponseEnd {
                            route_id: 52,
                            stream_id: 102,
                        } => second_ended = true,
                        MultiplexControlMessage::Error { message, .. } => {
                            panic!("shared route error: {message}")
                        }
                        other => panic!("unexpected shared route control: {other:?}"),
                    }
                }
                Message::Binary(frame) => {
                    let frame = decode_multiplex_binary_frame(frame.as_ref()).unwrap();
                    assert_eq!(frame.route_id, 52);
                    assert_eq!(frame.stream_id, 102);
                    assert_eq!(frame.kind, MultiplexBinaryFrameKind::ResponseBody);
                    second_body.extend_from_slice(&frame.payload);
                }
                Message::Ping(payload) => data1.send(Message::Pong(payload)).await.unwrap(),
                other => panic!("unexpected shared route response frame: {other:?}"),
            }
        }
        assert_eq!(String::from_utf8(second_body).unwrap(), "second:shared=ok");
        let _ = done_tx.send(());
        let _ = release_rx.await;
        drop((control, data1, data2, data3));
    });

    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, mut status_rx) = mpsc::channel(16);
    let (route_tx, route_rx) = mpsc::channel(8);
    let pool = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
        route_rx,
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match status_rx.recv().await {
                Some(Ok(())) => break,
                Some(Err(_)) => continue,
                None => panic!("split worker status channel closed"),
            }
        }
    })
    .await
    .expect("first split data connection timeout");
    let (route_response_tx, route_response_rx) = oneshot::channel();
    route_tx
        .send(SplitRouteCommand::Add {
            config: second_config,
            response: route_response_tx,
        })
        .await
        .expect("send shared route");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), route_response_rx)
            .await
            .expect("shared route registration timeout")
            .expect("shared route response")
            .expect("shared route registration"),
        (52, 16)
    );
    tokio::time::timeout(Duration::from_secs(8), done_rx)
        .await
        .expect("split scale and response timeout")
        .expect("split scale server finished");
    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.configured_workers, 8);
    assert_eq!(snapshot.connected_workers, 3);
    let _ = shutdown_tx.send(true);
    let _ = release_tx.send(());
    tokio::time::timeout(Duration::from_secs(5), pool)
        .await
        .expect("split pool shutdown timeout")
        .expect("split pool task");
    server.await.expect("split server");
    local_server.abort();
    let _ = local_server.await;
    second_local_server.abort();
    let _ = second_local_server.await;
}

#[tokio::test]
async fn multiplex_connection_forwards_two_concurrent_streams_on_one_socket() {
    use axum::extract::RawQuery;
    use axum::routing::any;
    use axum::Router;
    use tokio::net::TcpListener;

    let local_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local listener");
    let local_address = local_listener.local_addr().expect("local address");
    let local_server = tokio::spawn(async move {
        let app = Router::new().fallback(any(|RawQuery(query): RawQuery| async move {
            query.unwrap_or_else(|| "none".into())
        }));
        axum::serve(local_listener, app)
            .await
            .expect("local server");
    });

    let tunnel_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("tunnel listener");
    let tunnel_address = tunnel_listener.local_addr().expect("tunnel address");
    let device_key = SigningKey::from_bytes(&[34_u8; 32]);
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{tunnel_address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{tunnel_address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: format!("http://{local_address}"),
        device_id: "device-v5-streams".into(),
        signing_key: Arc::new(device_key),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: std::env::temp_dir().join("ct-v5-stream-test.log"),
    };

    let server = tokio::spawn(async move {
        let (stream, _) = tunnel_listener.accept().await.expect("accept tunnel");
        let captured = Arc::new(StdMutex::new(None::<(String, String)>));
        let captured_cb = captured.clone();
        let mut socket = tokio_tungstenite::accept_hdr_async(
            stream,
            move |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                  mut response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                let device_id = request.headers().get(DEVICE_ID_HEADER).and_then(|v| v.to_str().ok()).unwrap().to_string();
                let connection_id = request.headers().get(CONNECTION_ID_HEADER).and_then(|v| v.to_str().ok()).unwrap().to_string();
                *captured_cb.lock().unwrap() = Some((device_id, connection_id));
                response.headers_mut().insert(SEC_WEBSOCKET_PROTOCOL, MULTIPLEX_WS_SUBPROTOCOL.parse().unwrap());
                Ok(response)
            },
        ).await.expect("accept websocket");
        let (device_id, connection_id) = captured.lock().unwrap().clone().unwrap();
        let nonce = "rust-v5-stream-nonce".to_string();
        let expires_at_unix_ms = unix_ms().saturating_add(10_000);
        let server_key = test_server_signing_key();
        let challenge_signature = URL_SAFE_NO_PAD.encode(
            server_key
                .sign(&multiplex_server_challenge_signing_payload(
                    &nonce,
                    expires_at_unix_ms,
                    TEST_SERVER_ID,
                    &device_id,
                    &connection_id,
                ))
                .to_bytes(),
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::Challenge {
                    nonce: nonce.clone(),
                    expires_at_unix_ms,
                    server_id: TEST_SERVER_ID.into(),
                    server_signature: challenge_signature,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let authenticate = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::Authenticate(proof) =
            serde_json::from_str::<MultiplexControlMessage>(&authenticate).unwrap()
        else {
            panic!("authenticate")
        };
        let policy = MultiplexConnectionPolicy::default();
        let ack_signature = URL_SAFE_NO_PAD.encode(
            server_key
                .sign(&multiplex_server_ack_signing_payload(
                    &nonce,
                    TEST_SERVER_ID,
                    &proof,
                    &policy,
                ))
                .to_bytes(),
        );
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::HelloAck {
                    protocol_version: MULTIPLEX_PROTOCOL_VERSION,
                    connection_policy: policy,
                    server_id: TEST_SERVER_ID.into(),
                    server_signature: ack_signature,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let registration = socket.next().await.unwrap().unwrap().into_text().unwrap();
        let MultiplexControlMessage::RouteRegister {
            registration_id, ..
        } = serde_json::from_str::<MultiplexControlMessage>(&registration).unwrap()
        else {
            panic!("route register")
        };
        socket
            .send(Message::Text(
                serde_json::to_string(&MultiplexControlMessage::RouteRegistered {
                    registration_id,
                    route_id: 7,
                    stream_limit: 16,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();

        for stream_id in [1_u64, 2_u64] {
            socket
                .send(Message::Text(
                    serde_json::to_string(&MultiplexControlMessage::RequestHead {
                        route_id: 7,
                        stream_id,
                        method: "GET".into(),
                        path_and_query: format!("/builtin/clients/pc-a/mcp?stream={stream_id}"),
                        headers: vec![],
                        demand: None,
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Text(
                    serde_json::to_string(&MultiplexControlMessage::RequestEnd {
                        route_id: 7,
                        stream_id,
                    })
                    .unwrap()
                    .into(),
                ))
                .await
                .unwrap();
        }

        let mut bodies = std::collections::HashMap::<u64, Vec<u8>>::new();
        let mut ended = std::collections::HashSet::<u64>::new();
        while ended.len() < 2 {
            match tokio::time::timeout(Duration::from_secs(5), socket.next())
                .await
                .expect("response timeout")
                .expect("response frame")
                .expect("response")
            {
                Message::Text(text) => match serde_json::from_str::<MultiplexControlMessage>(&text)
                    .expect("response control")
                {
                    MultiplexControlMessage::ResponseHead {
                        route_id: 7,
                        stream_id,
                        status,
                        ..
                    } => {
                        assert!([1, 2].contains(&stream_id));
                        assert_eq!(status, 200);
                    }
                    MultiplexControlMessage::ResponseEnd {
                        route_id: 7,
                        stream_id,
                    } => {
                        ended.insert(stream_id);
                    }
                    MultiplexControlMessage::Error { message, .. } => {
                        panic!("multiplex error: {message}")
                    }
                    other => panic!("unexpected response control: {other:?}"),
                },
                Message::Binary(frame) => {
                    let frame =
                        decode_multiplex_binary_frame(frame.as_ref()).expect("response binary");
                    assert_eq!(frame.kind, MultiplexBinaryFrameKind::ResponseBody);
                    bodies
                        .entry(frame.stream_id)
                        .or_default()
                        .extend_from_slice(&frame.payload);
                }
                Message::Ping(payload) => socket.send(Message::Pong(payload)).await.unwrap(),
                other => panic!("unexpected websocket frame: {other:?}"),
            }
        }
        assert_eq!(
            String::from_utf8(bodies.remove(&1).unwrap()).unwrap(),
            "stream=1"
        );
        assert_eq!(
            String::from_utf8(bodies.remove(&2).unwrap()).unwrap(),
            "stream=2"
        );
        socket.close(None).await.unwrap();
    });

    let (_shutdown_tx, mut shutdown) = watch::channel(false);
    let (_retire_tx, mut retire) = watch::channel(false);
    let (status_tx, _status_rx) = mpsc::channel(8);
    let (event_tx, _event_rx) = mpsc::channel(32);
    let (policy_tx, _policy_rx) = watch::channel::<Option<WorkerPolicy>>(None);
    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let mut connected = false;
    let result = run_connected_worker(
        &config,
        "worker-streams",
        &mut shutdown,
        &mut retire,
        &status_tx,
        metrics,
        &mut connected,
        0,
        &event_tx,
        &policy_tx,
    )
    .await;
    assert!(connected);
    assert!(
        result.is_err()
            || matches!(
                result,
                Ok(WorkerConnectionExit::Shutdown | WorkerConnectionExit::ScaleDown)
            )
    );
    server.await.expect("tunnel server");
    local_server.abort();
    let _ = local_server.await;
}

#[test]
fn parses_namespaced_mcp_endpoint() {
    let endpoint = parse_builtin_endpoint(
        "https://tunnel.example.com/builtin/clients/pc-a/mcp",
        TunnelService::Mcp,
    )
    .unwrap();
    assert_eq!(endpoint.client_id, "pc-a");
    assert_eq!(endpoint.route_prefix, "/builtin/clients/pc-a");
    assert_eq!(
        endpoint.websocket_url,
        "wss://tunnel.example.com/_tunnel/v1"
    );
}

#[test]
fn upgrades_namespaced_mcp_base_url_to_endpoint() {
    let endpoint = parse_builtin_endpoint(
        "https://tunnel.example.com/builtin/clients/pc-a",
        TunnelService::Mcp,
    )
    .unwrap();
    assert_eq!(
        endpoint.public_url,
        "https://tunnel.example.com/builtin/clients/pc-a/mcp"
    );
    assert_eq!(endpoint.client_id, "pc-a");
}

#[test]
fn replaces_bootstrap_client_id_with_server_assigned_id() {
    let endpoint = builtin_endpoint_for_client(
        "https://tunnel.example.com/builtin/clients/workspace-placeholder/mcp",
        TunnelService::Mcp,
        "pc-a",
    )
    .unwrap();
    assert_eq!(
        endpoint.public_url,
        "https://tunnel.example.com/builtin/clients/pc-a/mcp"
    );
    assert_eq!(endpoint.client_id, "pc-a");
}

#[test]
fn actions_requests_strip_only_the_registered_prefix() {
    let config = BuiltinTunnelConfig {
        public_url: "https://example.com/builtin/actions/pc-a".into(),
        websocket_url: "wss://example.com/_tunnel/v1".into(),
        client_id: "pc-a".into(),
        service: TunnelService::Actions,
        route_prefix: "/builtin/actions/pc-a".into(),
        local_base_url: "http://127.0.0.1:7001".into(),
        device_id: "device-1".into(),
        signing_key: Arc::new(SigningKey::from_bytes(&[7_u8; 32])),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: PathBuf::new(),
    };
    assert_eq!(
        local_path_for_request(&config, "/builtin/actions/pc-a/openapi.json?x=1").unwrap(),
        "/openapi.json?x=1"
    );
    assert!(local_path_for_request(&config, "/builtin/actions/pc-ab").is_err());
}

#[test]
fn enrollment_link_must_match_the_public_origin_and_path() {
    let url = parse_enrollment_url(
        "https://tunnel.example.com/builtin/clients/pc-a/mcp",
        "https://tunnel.example.com/_tunnel/enroll/abc123",
    )
    .unwrap();
    assert_eq!(url.path(), "/_tunnel/enroll/abc123");
    assert!(parse_enrollment_url(
        "https://tunnel.example.com/builtin/clients/pc-a/mcp",
        "https://other.example/_tunnel/enroll/abc123",
    )
    .is_err());
    assert!(parse_enrollment_url(
        "https://tunnel.example.com/builtin/clients/pc-a/mcp",
        "https://tunnel.example.com/_tunnel/enroll/abc123?copy=1",
    )
    .is_err());
}

#[test]
fn rejects_non_namespaced_builtin_urls() {
    assert!(
        parse_builtin_endpoint("https://example.com/clients/pc-a/mcp", TunnelService::Mcp).is_err()
    );
}

#[test]
fn server_policy_updates_pool_metrics() {
    let metrics = BuiltinTunnelMetrics::new(1);
    let mut policy = WorkerPolicy::default_for(TunnelService::Mcp);
    policy.max_workers = 24;
    policy.revision = 7;
    metrics.set_policy(&policy);
    metrics.set_pool_counts(3, 2);

    let snapshot = metrics.snapshot();
    assert_eq!(snapshot.configured_workers, 24);
    assert_eq!(snapshot.idle_workers, 3);
    assert_eq!(snapshot.busy_workers, 2);
    assert_eq!(snapshot.policy_revision, 7);
}

#[test]
fn reconnect_backoff_is_jittered_bounded_and_resets_after_connect() {
    let base = Duration::from_secs(8);
    let worker_zero = reconnect_delay(base, 0, 3);
    let worker_one = reconnect_delay(base, 1, 3);

    assert!(worker_zero >= Duration::from_secs(4));
    assert!(worker_zero <= base);
    assert!(worker_one >= Duration::from_secs(4));
    assert!(worker_one <= base);
    assert_ne!(worker_zero, worker_one);
    assert_eq!(next_reconnect_base(base, true), ESTABLISHED_RECONNECT_DELAY);
    let established = reconnect_delay(ESTABLISHED_RECONNECT_DELAY, 0, 0);
    assert!(established >= Duration::from_millis(500));
    assert!(established <= ESTABLISHED_RECONNECT_DELAY);
    assert_eq!(
        next_reconnect_base(Duration::from_secs(10), false),
        Duration::from_secs(20)
    );
    assert_eq!(
        next_reconnect_base(Duration::from_secs(80), false),
        MAX_RECONNECT_DELAY
    );
}

#[test]
fn connected_worker_guard_keeps_live_count_exact() {
    let metrics = Arc::new(BuiltinTunnelMetrics::new(8));
    assert_eq!(metrics.snapshot().connected_workers, 0);

    let first = ConnectedWorkerGuard::new(metrics.clone());
    let second = ConnectedWorkerGuard::new(metrics.clone());
    assert_eq!(metrics.snapshot().connected_workers, 2);

    drop(first);
    assert_eq!(metrics.snapshot().connected_workers, 1);
    drop(second);
    assert_eq!(metrics.snapshot().connected_workers, 0);
    assert_eq!(metrics.snapshot().configured_workers, 8);
}

#[test]
fn availability_state_distinguishes_running_from_reconnecting() {
    let reconnecting = BuiltinTunnelSnapshot {
        configured_workers: 8,
        connected_workers: 0,
        idle_workers: 0,
        busy_workers: 0,
        recycled_workers: 0,
        policy_revision: 1,
        last_error: Some("offline".into()),
        transport_mode: Some("split-v5".into()),
        server_id: None,
        device_id: None,
        control_connected: false,
        session_id: None,
        route_id: None,
        stream_limit: None,
        streams_per_connection: None,
        active_streams: 0,
        queued_requests: 0,
        desired_data_connections: 0,
    };
    let running = BuiltinTunnelSnapshot {
        configured_workers: 8,
        connected_workers: 2,
        idle_workers: 1,
        busy_workers: 1,
        recycled_workers: 3,
        policy_revision: 2,
        last_error: None,
        transport_mode: Some("split-v5".into()),
        server_id: None,
        device_id: None,
        control_connected: true,
        session_id: Some("session-test".into()),
        route_id: Some(1),
        stream_limit: Some(16),
        streams_per_connection: Some(16),
        active_streams: 1,
        queued_requests: 0,
        desired_data_connections: 2,
    };

    assert_eq!(reconnecting.availability_state(true), "reconnecting");
    assert_eq!(running.availability_state(true), "running");
    assert_eq!(running.availability_state(false), "stopped");
}

#[test]
fn heartbeat_deadline_moves_forward_with_server_activity() {
    let started = Instant::now();
    let mut heartbeat = HeartbeatTracker::new_at(started);

    assert!(!heartbeat.expired_at(started + Duration::from_secs(89)));
    assert!(heartbeat.expired_at(started + Duration::from_secs(90)));

    heartbeat.record_activity_at(started + Duration::from_secs(30));
    assert!(!heartbeat.expired_at(started + Duration::from_secs(119)));
    assert!(heartbeat.expired_at(started + Duration::from_secs(120)));
}

#[test]
fn multiplex_pool_plan_matches_node_threshold_burst_and_scale_down() {
    let policy = coding_tools_tunnel_protocol::MultiplexConnectionPolicy::default();
    let cases = [
        (
            MultiplexPoolSnapshot {
                connected_connections: 1,
                connecting_connections: 0,
                active_streams: 11,
                queued_requests: 0,
                desired_connections: 1,
                idle_connections: 0,
                idle_excess_elapsed: false,
            },
            PoolAdjustment {
                spawn: 0,
                retire: 0,
            },
        ),
        (
            MultiplexPoolSnapshot {
                connected_connections: 1,
                connecting_connections: 0,
                active_streams: 12,
                queued_requests: 0,
                desired_connections: 1,
                idle_connections: 0,
                idle_excess_elapsed: false,
            },
            PoolAdjustment {
                spawn: 2,
                retire: 0,
            },
        ),
        (
            MultiplexPoolSnapshot {
                connected_connections: 1,
                connecting_connections: 0,
                active_streams: 16,
                queued_requests: 1,
                desired_connections: 2,
                idle_connections: 0,
                idle_excess_elapsed: false,
            },
            PoolAdjustment {
                spawn: 4,
                retire: 0,
            },
        ),
        (
            MultiplexPoolSnapshot {
                connected_connections: 1,
                connecting_connections: 2,
                active_streams: 12,
                queued_requests: 0,
                desired_connections: 1,
                idle_connections: 0,
                idle_excess_elapsed: false,
            },
            PoolAdjustment {
                spawn: 0,
                retire: 0,
            },
        ),
        (
            MultiplexPoolSnapshot {
                connected_connections: 5,
                connecting_connections: 0,
                active_streams: 0,
                queued_requests: 0,
                desired_connections: 1,
                idle_connections: 5,
                idle_excess_elapsed: false,
            },
            PoolAdjustment {
                spawn: 0,
                retire: 0,
            },
        ),
        (
            MultiplexPoolSnapshot {
                connected_connections: 5,
                connecting_connections: 0,
                active_streams: 0,
                queued_requests: 0,
                desired_connections: 1,
                idle_connections: 5,
                idle_excess_elapsed: true,
            },
            PoolAdjustment {
                spawn: 0,
                retire: 4,
            },
        ),
    ];
    for (snapshot, expected) in cases {
        assert_eq!(multiplex_pool_adjustment(&policy, snapshot), expected);
    }
}

#[test]
fn dynamic_pool_plan_uses_demand_connecting_limits_and_staged_shrink() {
    let mut policy = coding_tools_tunnel_protocol::WorkerPolicy::default_for(TunnelService::Mcp);
    policy.start_workers = 4;
    policy.min_idle_workers = 2;
    policy.max_idle_workers = 4;
    policy.max_workers = 16;
    policy.max_connecting_workers = 4;
    policy.scale_down_step = 4;
    policy.burst_warm_workers = 8;
    let max_connecting = configured_max_connecting(&policy);
    assert_eq!(max_connecting, 4);
    assert_eq!(configured_burst_warm_floor(&policy), 8);

    assert_eq!(
        pool_adjustment(
            &policy,
            PoolCounts {
                total: 1,
                connecting: 1,
                idle: 0,
                busy: 0,
            },
            1,
            max_connecting,
            0,
            false,
            4,
        ),
        PoolAdjustment {
            spawn: 3,
            retire: 0,
        }
    );
    assert_eq!(
        pool_adjustment(
            &policy,
            PoolCounts {
                total: 4,
                connecting: 0,
                idle: 1,
                busy: 3,
            },
            0,
            max_connecting,
            16,
            false,
            8,
        ),
        PoolAdjustment {
            spawn: 4,
            retire: 0,
        }
    );
    let connecting_limited = PoolCounts {
        total: 8,
        connecting: 4,
        idle: 0,
        busy: 4,
    };
    let connecting_adjustment =
        pool_adjustment(&policy, connecting_limited, 4, max_connecting, 16, false, 8);
    assert_eq!(connecting_adjustment.spawn, 0);
    assert_eq!(
        scale_up_block(
            &policy,
            connecting_limited,
            4,
            max_connecting,
            16,
            connecting_adjustment,
        ),
        Some(ScaleUpBlock::ConnectingLimitReached)
    );

    for (total, floor, expected_retire) in [(16, 8, 4), (12, 8, 4), (8, 8, 0), (8, 4, 4)] {
        assert_eq!(
            pool_adjustment(
                &policy,
                PoolCounts {
                    total,
                    connecting: 0,
                    idle: total,
                    busy: 0,
                },
                0,
                max_connecting,
                0,
                true,
                floor,
            )
            .retire,
            expected_retire,
        );
    }

    let maximum = PoolCounts {
        total: usize::from(policy.max_workers),
        connecting: 0,
        idle: 0,
        busy: usize::from(policy.max_workers),
    };
    assert_eq!(
        scale_up_block(
            &policy,
            maximum,
            0,
            max_connecting,
            usize::from(policy.max_workers).saturating_add(1),
            PoolAdjustment {
                spawn: 0,
                retire: 0,
            },
        ),
        Some(ScaleUpBlock::MaxWorkersReached)
    );
}

#[test]
fn stale_connecting_workers_stop_counting_as_idle_reserve() {
    let (_retire_tx, retire_rx) = watch::channel(false);
    let (second_tx, _second_rx) = watch::channel(false);
    let now = Instant::now();
    let workers = HashMap::from([
        (
            1,
            ManagedWorker {
                state: PoolWorkerState::Connecting,
                connecting_since: now,
                retire: _retire_tx,
            },
        ),
        (
            2,
            ManagedWorker {
                state: PoolWorkerState::Connecting,
                connecting_since: now - Duration::from_secs(2),
                retire: second_tx,
            },
        ),
    ]);
    drop(retire_rx);
    assert_eq!(
        effective_connecting_workers(&workers, Duration::from_secs(1)),
        1
    );
    let mut policy = WorkerPolicy::default_for(TunnelService::Mcp);
    policy.max_connecting_workers = 4;
    let counts = pool_counts(&workers);
    let adjustment = pool_adjustment(
        &policy,
        counts,
        1,
        configured_max_connecting(&policy),
        4,
        false,
        usize::from(policy.max_idle_workers),
    );
    assert_eq!(adjustment.spawn, 2);
}

#[test]
fn worker_recycle_limits_are_jittered_and_checked_only_at_idle_boundaries() {
    let policy = coding_tools_tunnel_protocol::WorkerPolicy::default_for(TunnelService::Mcp);
    let low = jittered_limit(
        policy.max_requests_per_worker,
        7,
        policy.recycle_jitter_percent,
    );
    let high = jittered_limit(
        policy.max_requests_per_worker,
        8,
        policy.recycle_jitter_percent,
    );
    assert!((4_000..=6_000).contains(&low));
    assert!((4_000..=6_000).contains(&high));
    assert_ne!(low, high);
    assert!(!worker_should_recycle(
        &policy,
        7,
        449,
        Duration::from_secs(10)
    ));
    assert!(worker_should_recycle(
        &policy,
        7,
        low,
        Duration::from_secs(10)
    ));
    assert!(worker_should_recycle(
        &policy,
        7,
        1,
        Duration::from_secs(jittered_limit(
            policy.max_lifetime_seconds,
            7,
            policy.recycle_jitter_percent,
        ))
    ));
}

#[tokio::test]
async fn worker_pool_bootstraps_grows_and_gracefully_shrinks_from_server_policy() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket test server");
    let address = listener.local_addr().expect("test server address");
    let mut grow_policy = single_worker_policy();
    grow_policy.start_workers = 3;
    grow_policy.min_idle_workers = 2;
    grow_policy.max_idle_workers = 3;
    grow_policy.max_workers = 3;
    grow_policy.max_connecting_workers = 3;
    grow_policy.scale_down_step = 3;
    grow_policy.burst_warm_workers = 3;
    let (policy_tx, policy_rx) = watch::channel(grow_policy.clone());
    let (ready_tx, mut ready_rx) = mpsc::channel(3);
    let (closed_tx, mut closed_rx) = mpsc::channel(3);
    let server = tokio::spawn(async move {
        let mut handlers = JoinSet::new();
        for connection_index in 0..3 {
            let (stream, _) = listener.accept().await.expect("accept worker");
            let ready_tx = ready_tx.clone();
            let closed_tx = closed_tx.clone();
            let mut updates = policy_rx.clone();
            handlers.spawn(async move {
                let initial_policy = updates.borrow().clone();
                let mut socket = accept_authenticated_test_worker(
                    stream,
                    format!("grow-{connection_index}"),
                    initial_policy,
                    &[WS_SUBPROTOCOL],
                )
                .await;
                let ready = socket.next().await.expect("ready frame").expect("ready");
                assert!(matches!(ready, Message::Text(_)));
                ready_tx.send(()).await.expect("report ready");

                updates.changed().await.expect("policy update");
                let updated_policy = updates.borrow().clone();
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::PolicyUpdate {
                            worker_policy: updated_policy,
                        })
                        .expect("policy update json")
                        .into(),
                    ))
                    .await
                    .expect("send policy update");
                while let Some(message) = socket.next().await {
                    if matches!(message, Ok(Message::Close(_))) {
                        break;
                    }
                }
                let _ = closed_tx.send(()).await;
            });
        }
        drop(ready_tx);
        drop(closed_tx);
        while handlers.join_next().await.is_some() {}
    });

    let log_dir = tempfile::tempdir().expect("log tempdir");
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: "http://127.0.0.1:1".into(),
        device_id: "device-1".into(),
        signing_key: Arc::new(SigningKey::from_bytes(&[29_u8; 32])),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: log_dir.path().join("builtin-grow-test.log"),
    };
    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, _status_rx) = mpsc::channel(8);
    let pool_log_path = config.log_path.clone();
    let (_route_tx, route_rx) = mpsc::channel(8);
    let pool = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
        route_rx,
    ));

    timeout(Duration::from_secs(4), async {
        for _ in 0..3 {
            ready_rx.recv().await.expect("worker ready");
        }
    })
    .await
    .expect("pool growth deadline");
    assert_eq!(metrics.snapshot().configured_workers, 3);
    assert_eq!(metrics.snapshot().connected_workers, 3);

    let mut shrink_policy = single_worker_policy();
    shrink_policy.revision = 2;
    policy_tx
        .send(shrink_policy)
        .expect("publish shrink policy");
    timeout(Duration::from_secs(4), async {
        while metrics.snapshot().connected_workers > 1 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        for _ in 0..2 {
            closed_rx.recv().await.expect("retired worker");
        }
    })
    .await
    .expect("pool shrink deadline");
    assert_eq!(metrics.snapshot().configured_workers, 1);
    assert_eq!(metrics.snapshot().connected_workers, 1);

    let _ = shutdown_tx.send(true);
    timeout(Duration::from_secs(2), pool)
        .await
        .expect("pool shutdown")
        .expect("pool task");
    timeout(Duration::from_secs(2), server)
        .await
        .expect("server shutdown")
        .expect("server task");
    let pool_log = std::fs::read_to_string(pool_log_path).expect("pool audit log");
    assert!(pool_log.contains("event=worker_policy_applied"));
    assert!(pool_log.contains("event=scale_up"));
    assert!(pool_log.contains("reason=startup"));
    assert!(pool_log.contains("event=scale_down"));
    assert!(pool_log.contains("reason=max_workers_reduced"));
}

#[tokio::test]
async fn worker_recycles_after_request_limit_and_pool_replaces_it() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local http server");
    let local_address = local_listener.local_addr().expect("local http address");
    let local_server = tokio::spawn(async move {
        let (mut stream, _) = local_listener.accept().await.expect("accept local request");
        let mut request = vec![0_u8; 4096];
        let read = stream.read(&mut request).await.expect("read local request");
        assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET "));
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .await
            .expect("write local response");
    });

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket test server");
    let address = listener.local_addr().expect("test server address");
    let mut recycle_policy = single_worker_policy();
    recycle_policy.max_requests_per_worker = 1;
    recycle_policy.max_lifetime_seconds = 0;
    recycle_policy.recycle_jitter_percent = 0;
    let (replacement_tx, mut replacement_rx) = mpsc::channel(1);
    let server = tokio::spawn(async move {
        for connection_index in 0..2 {
            let (stream, _) = listener.accept().await.expect("accept worker");
            let mut socket = accept_authenticated_test_worker(
                stream,
                format!("recycle-{connection_index}"),
                recycle_policy.clone(),
                &[WS_SUBPROTOCOL],
            )
            .await;
            let ready = socket.next().await.expect("ready frame").expect("ready");
            assert_eq!(
                serde_json::from_str::<ControlMessage>(ready.into_text().unwrap().as_ref())
                    .expect("ready json"),
                ControlMessage::Ready
            );

            if connection_index == 0 {
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::RequestHead {
                            request_id: "request-1".into(),
                            method: "GET".into(),
                            path_and_query: "/builtin/clients/pc-a/mcp".into(),
                            headers: Vec::new(),
                            demand: None,
                        })
                        .expect("request head")
                        .into(),
                    ))
                    .await
                    .expect("send request head");
                socket
                    .send(Message::Text(
                        serde_json::to_string(&ControlMessage::RequestEnd {
                            request_id: "request-1".into(),
                        })
                        .expect("request end")
                        .into(),
                    ))
                    .await
                    .expect("send request end");
                let mut response_finished = false;
                while let Some(message) = socket.next().await {
                    match message.expect("response frame") {
                        Message::Text(text) => {
                            let control = serde_json::from_str::<ControlMessage>(&text)
                                .expect("response control");
                            if matches!(control, ControlMessage::ResponseEnd { .. }) {
                                response_finished = true;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                assert!(response_finished);
            } else {
                replacement_tx.send(()).await.expect("replacement ready");
                while socket.next().await.is_some() {}
            }
        }
    });

    let log_dir = tempfile::tempdir().expect("log tempdir");
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: format!("http://{local_address}"),
        device_id: "device-1".into(),
        signing_key: Arc::new(SigningKey::from_bytes(&[31_u8; 32])),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: log_dir.path().join("builtin-recycle-test.log"),
    };
    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, _status_rx) = mpsc::channel(8);
    let (_route_tx, route_rx) = mpsc::channel(8);
    let pool = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
        route_rx,
    ));

    timeout(Duration::from_secs(4), replacement_rx.recv())
        .await
        .expect("replacement deadline")
        .expect("replacement worker");
    assert_eq!(metrics.snapshot().recycled_workers, 1);
    assert_eq!(metrics.snapshot().connected_workers, 1);

    let _ = shutdown_tx.send(true);
    timeout(Duration::from_secs(2), pool)
        .await
        .expect("pool shutdown")
        .expect("pool task");
    timeout(Duration::from_secs(2), server)
        .await
        .expect("server shutdown")
        .expect("server task");
    timeout(Duration::from_secs(2), local_server)
        .await
        .expect("local server shutdown")
        .expect("local server task");
}

#[tokio::test]
async fn worker_pool_reconnects_after_authenticated_socket_closes() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind websocket test server");
    let address = listener.local_addr().expect("test server address");
    let server = tokio::spawn(async move {
        for connection_index in 0..2 {
            let (stream, _) = listener.accept().await.expect("accept worker");
            let mut socket = accept_authenticated_test_worker(
                stream,
                format!("nonce-{connection_index}"),
                single_worker_policy(),
                &[WS_SUBPROTOCOL],
            )
            .await;
            let ready = socket
                .next()
                .await
                .expect("ready frame")
                .expect("ready frame");
            let Message::Text(ready) = ready else {
                panic!("expected ready text");
            };
            assert_eq!(
                serde_json::from_str::<ControlMessage>(ready.as_ref()).expect("ready json"),
                ControlMessage::Ready
            );

            if connection_index == 0 {
                socket.close(None).await.expect("close first socket");
            } else {
                while socket.next().await.is_some() {}
            }
        }
    });

    let log_dir = tempfile::tempdir().expect("log tempdir");
    let config = BuiltinTunnelConfig {
        public_url: format!("http://{address}/builtin/clients/pc-a/mcp"),
        websocket_url: format!("ws://{address}{WS_PATH}"),
        client_id: "pc-a".into(),
        service: TunnelService::Mcp,
        route_prefix: "/builtin/clients/pc-a".into(),
        local_base_url: "http://127.0.0.1:1".into(),
        device_id: "device-1".into(),
        signing_key: Arc::new(SigningKey::from_bytes(&[23_u8; 32])),
        server_id: TEST_SERVER_ID.into(),
        server_verifying_key: test_server_verifying_key(),
        log_path: log_dir.path().join("builtin-reconnect-test.log"),
    };
    let metrics = Arc::new(BuiltinTunnelMetrics::new(1));
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (status_tx, mut status_rx) = mpsc::channel(8);
    let (_route_tx, route_rx) = mpsc::channel(8);
    let pool = tokio::spawn(run_worker_pool(
        config,
        shutdown_rx,
        status_tx,
        metrics.clone(),
        route_rx,
    ));

    assert_eq!(status_rx.recv().await.expect("first connection"), Ok(()));
    let saw_reconnect = timeout(Duration::from_secs(3), async {
        let mut saw_disconnect = false;
        loop {
            match status_rx.recv().await.expect("worker status") {
                Ok(()) if saw_disconnect => return true,
                Ok(()) => {}
                Err(_) => saw_disconnect = true,
            }
        }
    })
    .await
    .expect("reconnect deadline");
    assert!(saw_reconnect);
    assert_eq!(metrics.snapshot().connected_workers, 1);

    let _ = shutdown_tx.send(true);
    timeout(Duration::from_secs(2), pool)
        .await
        .expect("pool shutdown")
        .expect("pool task");
    timeout(Duration::from_secs(2), server)
        .await
        .expect("server shutdown")
        .expect("server task");
}
