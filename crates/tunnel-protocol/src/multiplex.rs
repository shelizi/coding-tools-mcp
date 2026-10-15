use serde::{Deserialize, Serialize};

use crate::{HeaderPair, TunnelService, MAX_REQUEST_BODY_BYTES};

pub const MULTIPLEX_PROTOCOL_VERSION: u16 = 5;
pub const MULTIPLEX_WS_SUBPROTOCOL: &str = "coding-tools-tunnel-v5";
pub const MULTIPLEX_CONTROL_WS_SUBPROTOCOL: &str = "coding-tools-tunnel-v5-control";
pub const MULTIPLEX_DATA_WS_SUBPROTOCOL: &str = "coding-tools-tunnel-v5-data";
pub const CONNECTION_ID_HEADER: &str = "x-coding-tools-connection-id";
pub const SESSION_ID_HEADER: &str = "x-coding-tools-session-id";
pub const DEFAULT_MULTIPLEX_STREAMS_PER_CONNECTION: u16 = 16;
pub const MAX_MULTIPLEX_STREAMS_PER_CONNECTION: u16 = 64;
pub const MULTIPLEX_BINARY_HEADER_BYTES: usize = 14;
pub const MAX_MULTIPLEX_BINARY_PAYLOAD_BYTES: usize = MAX_REQUEST_BODY_BYTES;

pub type MultiplexRouteId = u32;
pub type MultiplexStreamId = u64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSessionHello {
    pub protocol_version: u16,
    pub connection_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceSessionAuthProof {
    pub hello: DeviceSessionHello,
    pub device_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MultiplexConnectionRole {
    Control,
    Data,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitSessionHello {
    pub protocol_version: u16,
    pub connection_role: MultiplexConnectionRole,
    pub connection_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SplitSessionAuthProof {
    pub hello: SplitSessionHello,
    pub device_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiplexConnectionPolicy {
    pub start_connections: u16,
    pub min_connections: u16,
    pub max_idle_connections: u16,
    pub max_connections: u16,
    pub streams_per_connection: u16,
    pub scale_up_threshold_percent: u8,
    pub scale_up_step: u16,
    pub burst_scale_up_step: u16,
    pub scale_down_delay_seconds: u64,
    pub revision: u64,
}

impl Default for MultiplexConnectionPolicy {
    fn default() -> Self {
        Self {
            start_connections: 1,
            min_connections: 1,
            max_idle_connections: 2,
            max_connections: 8,
            streams_per_connection: DEFAULT_MULTIPLEX_STREAMS_PER_CONNECTION,
            scale_up_threshold_percent: 75,
            scale_up_step: 2,
            burst_scale_up_step: 4,
            scale_down_delay_seconds: 90,
            revision: 1,
        }
    }
}

impl MultiplexConnectionPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if self.min_connections == 0
            || self.min_connections > self.start_connections
            || self.start_connections > self.max_idle_connections
            || self.max_idle_connections > self.max_connections
            || self.max_connections > 64
        {
            return Err(
                "connection counts must satisfy 1 <= min <= start <= max idle <= max <= 64".into(),
            );
        }
        if self.streams_per_connection == 0
            || self.streams_per_connection > MAX_MULTIPLEX_STREAMS_PER_CONNECTION
        {
            return Err(format!(
                "streams per connection must be between 1 and {MAX_MULTIPLEX_STREAMS_PER_CONNECTION}"
            ));
        }
        if !(50..=95).contains(&self.scale_up_threshold_percent) {
            return Err("scale-up threshold must be between 50 and 95 percent".into());
        }
        if self.scale_up_step == 0 || self.scale_up_step > self.max_connections {
            return Err("scale-up step must be between 1 and max connections".into());
        }
        if self.burst_scale_up_step < self.scale_up_step
            || self.burst_scale_up_step > self.max_connections
        {
            return Err(
                "burst scale-up step must be between scale-up step and max connections".into(),
            );
        }
        if self.scale_down_delay_seconds > 3_600 {
            return Err("scale-down delay must be between 0 and 3600 seconds".into());
        }
        if self.revision == 0 {
            return Err("connection policy revision must be positive".into());
        }
        Ok(())
    }
}

#[derive(Serialize)]
struct DeviceSessionAuthSigningPayload<'a> {
    protocol_version: u16,
    nonce: &'a str,
    device_id: &'a str,
    connection_id: &'a str,
}

pub fn device_session_auth_signing_payload(nonce: &str, proof: &DeviceSessionAuthProof) -> Vec<u8> {
    serde_json::to_vec(&DeviceSessionAuthSigningPayload {
        protocol_version: proof.hello.protocol_version,
        nonce,
        device_id: &proof.device_id,
        connection_id: &proof.hello.connection_id,
    })
    .expect("device session auth signing payload is serializable")
}

#[derive(Serialize)]
struct SplitSessionAuthSigningPayload<'a> {
    protocol_version: u16,
    nonce: &'a str,
    connection_role: MultiplexConnectionRole,
    device_id: &'a str,
    connection_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<&'a str>,
}

pub fn split_session_auth_signing_payload(nonce: &str, proof: &SplitSessionAuthProof) -> Vec<u8> {
    serde_json::to_vec(&SplitSessionAuthSigningPayload {
        protocol_version: proof.hello.protocol_version,
        nonce,
        connection_role: proof.hello.connection_role,
        device_id: &proof.device_id,
        connection_id: &proof.hello.connection_id,
        session_id: proof.hello.session_id.as_deref(),
    })
    .expect("split session auth signing payload is serializable")
}

#[derive(Serialize)]
struct SplitServerAckSigningPayload<'a> {
    protocol_version: u16,
    nonce: &'a str,
    server_id: &'a str,
    connection_role: MultiplexConnectionRole,
    device_id: &'a str,
    connection_id: &'a str,
    session_id: &'a str,
    connection_policy: &'a MultiplexConnectionPolicy,
}

pub fn split_server_ack_signing_payload(
    nonce: &str,
    server_id: &str,
    proof: &SplitSessionAuthProof,
    session_id: &str,
    connection_policy: &MultiplexConnectionPolicy,
) -> Vec<u8> {
    serde_json::to_vec(&SplitServerAckSigningPayload {
        protocol_version: MULTIPLEX_PROTOCOL_VERSION,
        nonce,
        server_id,
        connection_role: proof.hello.connection_role,
        device_id: &proof.device_id,
        connection_id: &proof.hello.connection_id,
        session_id,
        connection_policy,
    })
    .expect("split server ack signing payload is serializable")
}

#[derive(Serialize)]
struct MultiplexServerChallengeSigningPayload<'a> {
    protocol_version: u16,
    nonce: &'a str,
    expires_at_unix_ms: u64,
    server_id: &'a str,
    device_id: &'a str,
    connection_id: &'a str,
}

pub fn multiplex_server_challenge_signing_payload(
    nonce: &str,
    expires_at_unix_ms: u64,
    server_id: &str,
    device_id: &str,
    connection_id: &str,
) -> Vec<u8> {
    serde_json::to_vec(&MultiplexServerChallengeSigningPayload {
        protocol_version: MULTIPLEX_PROTOCOL_VERSION,
        nonce,
        expires_at_unix_ms,
        server_id,
        device_id,
        connection_id,
    })
    .expect("multiplex server challenge signing payload is serializable")
}

#[derive(Serialize)]
struct MultiplexServerAckSigningPayload<'a> {
    protocol_version: u16,
    nonce: &'a str,
    server_id: &'a str,
    device_id: &'a str,
    connection_id: &'a str,
    connection_policy: &'a MultiplexConnectionPolicy,
}

pub fn multiplex_server_ack_signing_payload(
    nonce: &str,
    server_id: &str,
    proof: &DeviceSessionAuthProof,
    connection_policy: &MultiplexConnectionPolicy,
) -> Vec<u8> {
    serde_json::to_vec(&MultiplexServerAckSigningPayload {
        protocol_version: MULTIPLEX_PROTOCOL_VERSION,
        nonce,
        server_id,
        device_id: &proof.device_id,
        connection_id: &proof.hello.connection_id,
        connection_policy,
    })
    .expect("multiplex server ack signing payload is serializable")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiplexDemand {
    pub queued_requests: u16,
    pub oldest_queue_wait_ms: u64,
    pub active_streams: u16,
    pub available_stream_slots: u16,
    pub desired_connections: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MultiplexControlMessage {
    Challenge {
        nonce: String,
        expires_at_unix_ms: u64,
        server_id: String,
        server_signature: String,
    },
    Authenticate(DeviceSessionAuthProof),
    SessionAuthenticate(SplitSessionAuthProof),
    HelloAck {
        protocol_version: u16,
        connection_policy: MultiplexConnectionPolicy,
        server_id: String,
        server_signature: String,
    },
    SessionReady {
        protocol_version: u16,
        connection_role: MultiplexConnectionRole,
        session_id: String,
        connection_policy: MultiplexConnectionPolicy,
        server_id: String,
        server_signature: String,
    },
    CapacityUpdate {
        demand: MultiplexDemand,
    },
    RouteRegister {
        registration_id: String,
        client_id: String,
        service: TunnelService,
    },
    RouteRegistered {
        registration_id: String,
        route_id: MultiplexRouteId,
        stream_limit: u16,
    },
    RouteUnregister {
        route_id: MultiplexRouteId,
    },
    RequestHead {
        route_id: MultiplexRouteId,
        stream_id: MultiplexStreamId,
        method: String,
        path_and_query: String,
        headers: Vec<HeaderPair>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        demand: Option<MultiplexDemand>,
    },
    RequestEnd {
        route_id: MultiplexRouteId,
        stream_id: MultiplexStreamId,
    },
    ResponseHead {
        route_id: MultiplexRouteId,
        stream_id: MultiplexStreamId,
        status: u16,
        headers: Vec<HeaderPair>,
    },
    ResponseEnd {
        route_id: MultiplexRouteId,
        stream_id: MultiplexStreamId,
    },
    Cancel {
        route_id: MultiplexRouteId,
        stream_id: MultiplexStreamId,
    },
    Error {
        route_id: Option<MultiplexRouteId>,
        stream_id: Option<MultiplexStreamId>,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MultiplexBinaryFrameKind {
    RequestBody = 1,
    ResponseBody = 2,
}

impl MultiplexBinaryFrameKind {
    fn from_byte(value: u8) -> Result<Self, String> {
        match value {
            1 => Ok(Self::RequestBody),
            2 => Ok(Self::ResponseBody),
            _ => Err(format!("unknown multiplex binary frame kind {value}")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiplexBinaryFrame {
    pub kind: MultiplexBinaryFrameKind,
    pub route_id: MultiplexRouteId,
    pub stream_id: MultiplexStreamId,
    pub payload: Vec<u8>,
}

fn validate_multiplex_ids(
    route_id: MultiplexRouteId,
    stream_id: MultiplexStreamId,
) -> Result<(), String> {
    if route_id == 0 {
        return Err("multiplex route id must be positive".into());
    }
    if stream_id == 0 {
        return Err("multiplex stream id must be positive".into());
    }
    Ok(())
}

pub fn encode_multiplex_binary_frame(
    kind: MultiplexBinaryFrameKind,
    route_id: MultiplexRouteId,
    stream_id: MultiplexStreamId,
    payload: &[u8],
) -> Result<Vec<u8>, String> {
    validate_multiplex_ids(route_id, stream_id)?;
    if payload.len() > MAX_MULTIPLEX_BINARY_PAYLOAD_BYTES {
        return Err(format!(
            "multiplex binary payload exceeds {} bytes",
            MAX_MULTIPLEX_BINARY_PAYLOAD_BYTES
        ));
    }

    let mut frame = Vec::with_capacity(MULTIPLEX_BINARY_HEADER_BYTES + payload.len());
    frame.push(MULTIPLEX_PROTOCOL_VERSION as u8);
    frame.push(kind as u8);
    frame.extend_from_slice(&route_id.to_be_bytes());
    frame.extend_from_slice(&stream_id.to_be_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn decode_multiplex_binary_frame(frame: &[u8]) -> Result<MultiplexBinaryFrame, String> {
    if frame.len() < MULTIPLEX_BINARY_HEADER_BYTES {
        return Err(format!(
            "multiplex binary frame must be at least {MULTIPLEX_BINARY_HEADER_BYTES} bytes"
        ));
    }
    if frame[0] != MULTIPLEX_PROTOCOL_VERSION as u8 {
        return Err(format!(
            "unsupported multiplex binary protocol version {}",
            frame[0]
        ));
    }

    let kind = MultiplexBinaryFrameKind::from_byte(frame[1])?;
    let route_id = u32::from_be_bytes(
        frame[2..6]
            .try_into()
            .expect("multiplex route id slice has fixed length"),
    );
    let stream_id = u64::from_be_bytes(
        frame[6..14]
            .try_into()
            .expect("multiplex stream id slice has fixed length"),
    );
    validate_multiplex_ids(route_id, stream_id)?;

    let payload = &frame[MULTIPLEX_BINARY_HEADER_BYTES..];
    if payload.len() > MAX_MULTIPLEX_BINARY_PAYLOAD_BYTES {
        return Err(format!(
            "multiplex binary payload exceeds {} bytes",
            MAX_MULTIPLEX_BINARY_PAYLOAD_BYTES
        ));
    }

    Ok(MultiplexBinaryFrame {
        kind,
        route_id,
        stream_id,
        payload: payload.to_vec(),
    })
}
