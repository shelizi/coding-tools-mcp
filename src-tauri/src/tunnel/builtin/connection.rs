use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use coding_tools_tunnel_protocol::{
    auth_signing_payload, ClientHello, ControlMessage, DeviceAuthProof, WorkerPolicy,
    CLIENT_ID_HEADER, PROTOCOL_VERSION, SERVICE_HEADER, WS_SUBPROTOCOL,
};
use ed25519_dalek::Signer;
use futures_util::StreamExt;
use tokio::time::timeout;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::SEC_WEBSOCKET_PROTOCOL;

use super::protocol_io::{receive_control, send_control, ClientSink, ClientStream};
use super::{BuiltinTunnelConfig, WEBSOCKET_CONNECT_TIMEOUT};

pub(super) struct AuthenticatedWorkerConnection {
    pub(super) sink: ClientSink,
    pub(super) stream: ClientStream,
    pub(super) initial_policy: WorkerPolicy,
}

pub(super) async fn connect_authenticated_worker(
    config: &BuiltinTunnelConfig,
    worker_id: &str,
) -> Result<AuthenticatedWorkerConnection, String> {
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
        SEC_WEBSOCKET_PROTOCOL,
        WS_SUBPROTOCOL
            .parse()
            .map_err(|error| format!("invalid tunnel subprotocol: {error}"))?,
    );

    let (socket, response) = timeout(WEBSOCKET_CONNECT_TIMEOUT, connect_async(request))
        .await
        .map_err(|_| "WSS connection timed out".to_string())?
        .map_err(|error| error.to_string())?;
    if response
        .headers()
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())
        != Some(WS_SUBPROTOCOL)
    {
        return Err("server did not accept coding-tools-tunnel-v3".into());
    }

    let (mut sink, mut stream) = socket.split();
    let (nonce, expires_at_unix_ms) = match receive_control(&mut sink, &mut stream).await? {
        ControlMessage::Challenge {
            nonce,
            expires_at_unix_ms,
        } => (nonce, expires_at_unix_ms),
        ControlMessage::Error { message, .. } => return Err(message),
        _ => return Err("server did not issue a device authentication challenge".into()),
    };
    if unix_ms() > expires_at_unix_ms {
        return Err("server authentication challenge already expired".into());
    }

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
    send_control(&mut sink, &ControlMessage::Authenticate(proof)).await?;

    let initial_policy = match receive_control(&mut sink, &mut stream).await? {
        ControlMessage::HelloAck {
            protocol_version,
            worker_policy,
        } if protocol_version == PROTOCOL_VERSION => worker_policy,
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

pub(super) fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
