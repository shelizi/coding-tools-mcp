use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 3;
pub const WS_PATH: &str = "/_tunnel/v1";
pub const ENROLL_PATH_PREFIX: &str = "/_tunnel/enroll";
pub const WS_SUBPROTOCOL: &str = "coding-tools-tunnel-v3";
pub const CLIENT_ID_HEADER: &str = "x-coding-tools-client-id";
pub const SERVICE_HEADER: &str = "x-coding-tools-service";
pub const BUILTIN_MCP_PREFIX: &str = "/builtin/clients";
pub const BUILTIN_ACTIONS_PREFIX: &str = "/builtin/actions";
pub const MAX_REQUEST_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TunnelService {
    Mcp,
    Actions,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkerPolicy {
    pub start_workers: u16,
    pub min_idle_workers: u16,
    pub max_idle_workers: u16,
    pub max_workers: u16,
    pub max_requests_per_worker: u64,
    pub max_lifetime_seconds: u64,
    pub scale_down_delay_seconds: u64,
    pub recycle_jitter_percent: u8,
    pub revision: u64,
}

impl WorkerPolicy {
    pub fn default_for(_service: TunnelService) -> Self {
        Self {
            start_workers: 4,
            min_idle_workers: 2,
            max_idle_workers: 4,
            max_workers: 16,
            max_requests_per_worker: 500,
            max_lifetime_seconds: 3_600,
            scale_down_delay_seconds: 60,
            recycle_jitter_percent: 10,
            revision: 1,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.min_idle_workers == 0
            || self.min_idle_workers > self.start_workers
            || self.start_workers > self.max_idle_workers
            || self.max_idle_workers > self.max_workers
            || self.max_workers > 256
        {
            return Err(
                "worker counts must satisfy 1 <= min idle <= start <= max idle <= max workers <= 256"
                    .into(),
            );
        }
        if self.max_requests_per_worker > 1_000_000 {
            return Err("max requests per worker must be between 0 and 1000000".into());
        }
        if self.max_lifetime_seconds != 0
            && !(60..=7 * 24 * 60 * 60).contains(&self.max_lifetime_seconds)
        {
            return Err("max lifetime must be 0 or between 60 and 604800 seconds".into());
        }
        if self.scale_down_delay_seconds > 3_600 {
            return Err("scale-down delay must be between 0 and 3600 seconds".into());
        }
        if self.recycle_jitter_percent > 50 {
            return Err("recycle jitter must be between 0 and 50 percent".into());
        }
        if self.revision == 0 {
            return Err("worker policy revision must be positive".into());
        }
        Ok(())
    }
}

impl TunnelService {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Actions => "actions",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "mcp" => Some(Self::Mcp),
            "actions" => Some(Self::Actions),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeaderPair {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub protocol_version: u16,
    pub client_id: String,
    pub service: TunnelService,
    pub worker_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceAuthProof {
    pub hello: ClientHello,
    pub device_id: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentRequest {
    pub device_id: String,
    pub client_id: String,
    pub device_name: String,
    pub public_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnrollmentResponse {
    pub device_id: String,
    #[serde(default)]
    pub client_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlMessage {
    Challenge {
        nonce: String,
        expires_at_unix_ms: u64,
    },
    Authenticate(DeviceAuthProof),
    HelloAck {
        protocol_version: u16,
        worker_policy: WorkerPolicy,
    },
    PolicyUpdate {
        worker_policy: WorkerPolicy,
    },
    Ready,
    RequestHead {
        request_id: String,
        method: String,
        path_and_query: String,
        headers: Vec<HeaderPair>,
    },
    RequestEnd {
        request_id: String,
    },
    ResponseHead {
        request_id: String,
        status: u16,
        headers: Vec<HeaderPair>,
    },
    ResponseEnd {
        request_id: String,
    },
    Cancel {
        request_id: String,
    },
    Error {
        request_id: Option<String>,
        message: String,
    },
}

#[derive(Serialize)]
struct AuthSigningPayload<'a> {
    protocol_version: u16,
    nonce: &'a str,
    device_id: &'a str,
    client_id: &'a str,
    service: TunnelService,
    worker_id: &'a str,
}

pub fn auth_signing_payload(nonce: &str, proof: &DeviceAuthProof) -> Vec<u8> {
    serde_json::to_vec(&AuthSigningPayload {
        protocol_version: proof.hello.protocol_version,
        nonce,
        device_id: &proof.device_id,
        client_id: &proof.hello.client_id,
        service: proof.hello.service,
        worker_id: &proof.hello.worker_id,
    })
    .expect("auth signing payload is serializable")
}

pub fn valid_client_id(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn expected_routes(client_id: &str, service: TunnelService) -> Vec<String> {
    match service {
        TunnelService::Mcp => {
            let route_prefix = format!("{BUILTIN_MCP_PREFIX}/{client_id}");
            vec![
                route_prefix.clone(),
                format!("/.well-known/oauth-authorization-server{route_prefix}"),
                format!("/.well-known/oauth-protected-resource{route_prefix}/mcp"),
            ]
        }
        TunnelService::Actions => vec![format!("{BUILTIN_ACTIONS_PREFIX}/{client_id}")],
    }
}

pub fn route_matches(prefix: &str, path: &str) -> bool {
    path == prefix
        || path
            .strip_prefix(prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_ids_are_path_safe() {
        assert!(valid_client_id("pc-a_01"));
        assert!(!valid_client_id(""));
        assert!(!valid_client_id("../pc"));
        assert!(!valid_client_id("pc/a"));
    }

    #[test]
    fn mcp_routes_include_standard_well_known_paths() {
        assert_eq!(
            expected_routes("pc-a", TunnelService::Mcp),
            vec![
                "/builtin/clients/pc-a",
                "/.well-known/oauth-authorization-server/builtin/clients/pc-a",
                "/.well-known/oauth-protected-resource/builtin/clients/pc-a/mcp",
            ]
        );
    }

    #[test]
    fn route_matching_respects_segment_boundaries() {
        assert!(route_matches(
            "/builtin/actions/pc-a",
            "/builtin/actions/pc-a/openapi.json"
        ));
        assert!(!route_matches(
            "/builtin/actions/pc-a",
            "/builtin/actions/pc-ab"
        ));
    }

    #[test]
    fn control_messages_round_trip() {
        let message = ControlMessage::Authenticate(DeviceAuthProof {
            hello: ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_id: "pc-a".into(),
                service: TunnelService::Mcp,
                worker_id: "worker-1".into(),
            },
            device_id: "device-1".into(),
            signature: "signature".into(),
        });
        let encoded = serde_json::to_string(&message).unwrap();
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&encoded).unwrap(),
            message
        );
    }

    #[test]
    fn signing_payload_is_stable_and_binds_the_worker_identity() {
        let proof = DeviceAuthProof {
            hello: ClientHello {
                protocol_version: PROTOCOL_VERSION,
                client_id: "pc-a".into(),
                service: TunnelService::Mcp,
                worker_id: "worker-1".into(),
            },
            device_id: "device-1".into(),
            signature: String::new(),
        };
        assert_eq!(
            String::from_utf8(auth_signing_payload("nonce-1", &proof)).unwrap(),
            r#"{"protocol_version":3,"nonce":"nonce-1","device_id":"device-1","client_id":"pc-a","service":"mcp","worker_id":"worker-1"}"#
        );
    }

    #[test]
    fn protocol_v3_carries_the_authoritative_worker_policy() {
        assert_eq!(PROTOCOL_VERSION, 3);
        assert_eq!(WS_SUBPROTOCOL, "coding-tools-tunnel-v3");

        let policy = WorkerPolicy::default_for(TunnelService::Mcp);
        assert_eq!(policy.start_workers, 4);
        assert_eq!(policy.min_idle_workers, 2);
        assert_eq!(policy.max_idle_workers, 4);
        assert_eq!(policy.max_workers, 16);
        assert_eq!(policy.max_requests_per_worker, 500);
        assert_eq!(policy.max_lifetime_seconds, 3_600);
        assert_eq!(policy.scale_down_delay_seconds, 60);
        assert_eq!(policy.recycle_jitter_percent, 10);
        assert_eq!(policy.revision, 1);

        let message = ControlMessage::HelloAck {
            protocol_version: PROTOCOL_VERSION,
            worker_policy: policy.clone(),
        };
        let encoded = serde_json::to_string(&message).expect("policy hello ack");
        assert_eq!(
            serde_json::from_str::<ControlMessage>(&encoded).unwrap(),
            message
        );
        assert!(matches!(
            ControlMessage::PolicyUpdate {
                worker_policy: policy,
            },
            ControlMessage::PolicyUpdate { .. }
        ));
    }

    #[test]
    fn worker_policy_rejects_invalid_fpm_relationships_and_bounds() {
        let valid = WorkerPolicy::default_for(TunnelService::Actions);
        assert!(valid.validate().is_ok());

        let mut invalid = valid.clone();
        invalid.min_idle_workers = invalid.start_workers + 1;
        assert!(invalid.validate().is_err());

        invalid = valid.clone();
        invalid.max_idle_workers = invalid.max_workers + 1;
        assert!(invalid.validate().is_err());

        invalid = valid;
        invalid.recycle_jitter_percent = 51;
        assert!(invalid.validate().is_err());
    }
}
