use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use coding_tools_tunnel_protocol::{
    valid_client_id, EnrollmentRequest, EnrollmentResponse, ENROLL_PATH_PREFIX,
};
use ed25519_dalek::{SigningKey, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::secret::SecretStore;

const DEVICE_IDENTITY_KEY: &str = "builtin_tunnel_device_identity";
const DEVICE_IDENTITY_APP_SCOPE: &str = "builtin_tunnel_device_identity";
const ROUTE_BINDING_KEY: &str = "builtin_tunnel_route_binding";
const ENROLLMENT_URL_KEY: &str = "builtin_tunnel_enrollment_url";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredDeviceIdentity {
    pub(super) device_id: String,
    pub(super) client_id: String,
    pub(super) private_key: String,
    enrolled: bool,
    #[serde(default)]
    pub(super) server_id: String,
    #[serde(default)]
    pub(super) server_public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct StoredRouteBinding {
    client_id: String,
    server_id: String,
}

fn server_identity_scope_id(public_url: &str) -> AppResult<String> {
    let url = reqwest::Url::parse(public_url)
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    let host = url
        .host_str()
        .ok_or_else(|| AppError::Message("內建隧道公開網址缺少主機名稱。".into()))?
        .to_ascii_lowercase();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AppError::Message("內建隧道公開網址缺少有效連接埠。".into()))?;
    let origin = format!("{}://{host}:{port}", url.scheme().to_ascii_lowercase());
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(origin.as_bytes())))
}

fn resolved_identity(
    identity: &StoredDeviceIdentity,
    binding: &StoredRouteBinding,
) -> StoredDeviceIdentity {
    let mut resolved = identity.clone();
    resolved.client_id = binding.client_id.clone();
    resolved
}

fn decode_identity(raw: String) -> AppResult<StoredDeviceIdentity> {
    serde_json::from_str::<StoredDeviceIdentity>(&raw)
        .map_err(|error| AppError::Message(format!("內建隧道裝置身分格式損壞：{error}")))
}

fn load_route_binding(profile_id: &str) -> AppResult<Option<StoredRouteBinding>> {
    SecretStore::get(profile_id, ROUTE_BINDING_KEY)?
        .filter(|raw| !raw.trim().is_empty())
        .map(|raw| {
            serde_json::from_str::<StoredRouteBinding>(&raw)
                .map_err(|error| AppError::Message(format!("內建隧道路由綁定格式損壞：{error}")))
        })
        .transpose()
}

fn save_route_binding(profile_id: &str, binding: &StoredRouteBinding) -> AppResult<()> {
    SecretStore::set(
        profile_id,
        ROUTE_BINDING_KEY,
        &serde_json::to_string(binding)?,
    )
}

fn load_server_identity(scope_id: &str) -> AppResult<Option<StoredDeviceIdentity>> {
    SecretStore::get_app(DEVICE_IDENTITY_APP_SCOPE, scope_id)?
        .map(decode_identity)
        .transpose()
}

fn save_server_identity(scope_id: &str, identity: &StoredDeviceIdentity) -> AppResult<()> {
    SecretStore::set_app(
        DEVICE_IDENTITY_APP_SCOPE,
        scope_id,
        &serde_json::to_string(identity)?,
    )
}

fn expected_server_id(public_key: &str) -> AppResult<String> {
    let key = decode_server_verifying_key(public_key)?;
    Ok(URL_SAFE_NO_PAD.encode(Sha256::digest(key.to_bytes())))
}

fn has_pinned_server_identity(identity: &StoredDeviceIdentity) -> bool {
    identity.server_id.len() == 43
        && identity
            .server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        && expected_server_id(&identity.server_public_key)
            .is_ok_and(|expected| expected == identity.server_id)
}

pub(super) async fn load_or_enroll_device_identity(
    profile_id: &str,
    public_url: &str,
    client_id: &str,
) -> AppResult<StoredDeviceIdentity> {
    let scope_id = server_identity_scope_id(public_url)?;
    let legacy = SecretStore::get(profile_id, DEVICE_IDENTITY_KEY)?
        .map(decode_identity)
        .transpose()?;
    let mut binding = load_route_binding(profile_id)?;
    let mut identity = load_server_identity(&scope_id)?;

    if identity.is_none() {
        if let Some(legacy_identity) = legacy
            .as_ref()
            .filter(|identity| identity.enrolled && has_pinned_server_identity(identity))
        {
            save_server_identity(&scope_id, legacy_identity)?;
            if binding.is_none() && valid_client_id(&legacy_identity.client_id) {
                let migrated = StoredRouteBinding {
                    client_id: legacy_identity.client_id.clone(),
                    server_id: legacy_identity.server_id.clone(),
                };
                save_route_binding(profile_id, &migrated)?;
                binding = Some(migrated);
            }
            identity = Some(legacy_identity.clone());
        }
    }

    let mut identity = match identity {
        Some(identity) => identity,
        None => {
            let signing_key = SigningKey::generate(&mut OsRng);
            let identity = StoredDeviceIdentity {
                device_id: Uuid::new_v4().simple().to_string(),
                client_id: client_id.to_string(),
                private_key: URL_SAFE_NO_PAD.encode(signing_key.to_bytes()),
                enrolled: false,
                server_id: String::new(),
                server_public_key: String::new(),
            };
            save_server_identity(&scope_id, &identity)?;
            identity
        }
    };

    if identity.enrolled && has_pinned_server_identity(&identity) {
        if let Some(binding) = binding.as_ref().filter(|binding| {
            binding.server_id == identity.server_id && valid_client_id(&binding.client_id)
        }) {
            return Ok(resolved_identity(&identity, binding));
        }
    }

    let enrollment_url = SecretStore::get(profile_id, ENROLLMENT_URL_KEY)?
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            if identity.enrolled {
                AppError::Message(
                    "此 Workspace 尚未授權使用目前的內建 WSS 裝置身分。請貼上伺服器產生的一次性註冊連結。"
                        .into(),
                )
            } else {
                AppError::Message(
                    "內建 WSS 隧道尚未註冊。請貼上伺服器產生的一次性註冊連結。".into(),
                )
            }
        })?;
    let enrollment_url = parse_enrollment_url(public_url, &enrollment_url)?;
    let signing_key = decode_signing_key(&identity.private_key)?;
    let request = EnrollmentRequest {
        device_id: identity.device_id.clone(),
        client_id: client_id.to_string(),
        device_name: local_device_name(),
        public_key: URL_SAFE_NO_PAD.encode(signing_key.verifying_key().to_bytes()),
    };
    let response = reqwest::Client::builder()
        .connect_timeout(super::WEBSOCKET_CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| AppError::Message(format!("無法建立裝置註冊連線：{error}")))?
        .post(enrollment_url)
        .json(&request)
        .send()
        .await
        .map_err(|error| AppError::Message(format!("裝置註冊連線失敗：{error}")))?;
    if !response.status().is_success() {
        let status = response.status();
        let detail = response.text().await.unwrap_or_default();
        return Err(AppError::Message(format!(
            "內建隧道裝置註冊失敗（{status}）：{}",
            detail.trim()
        )));
    }
    let enrolled = response
        .json::<EnrollmentResponse>()
        .await
        .map_err(|error| AppError::Message(format!("裝置註冊回應格式無效：{error}")))?;
    if enrolled.device_id != identity.device_id {
        return Err(AppError::Message(
            "內建隧道伺服器回傳了不一致的裝置 ID。".into(),
        ));
    }
    let enrolled_client_id = if enrolled.client_id.trim().is_empty() {
        client_id.to_string()
    } else {
        enrolled.client_id.trim().to_string()
    };
    if !valid_client_id(&enrolled_client_id) {
        return Err(AppError::Message(
            "內建隧道伺服器回傳了無效的 Client ID。".into(),
        ));
    }
    if enrolled.server_id.len() != 43
        || !enrolled
            .server_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || expected_server_id(&enrolled.server_public_key)
            .map_or(true, |expected| expected != enrolled.server_id)
    {
        return Err(AppError::Message(
            "內建隧道伺服器回傳了無效的伺服器身分。".into(),
        ));
    }
    if identity.enrolled
        && has_pinned_server_identity(&identity)
        && (identity.server_id != enrolled.server_id
            || identity.server_public_key != enrolled.server_public_key)
    {
        return Err(AppError::Message(
            "同一內建隧道伺服器來源回傳了不同的伺服器身分，已拒絕覆寫既有裝置綁定。".into(),
        ));
    }

    identity.client_id = enrolled_client_id.clone();
    identity.enrolled = true;
    identity.server_id = enrolled.server_id.clone();
    identity.server_public_key = enrolled.server_public_key;
    save_server_identity(&scope_id, &identity)?;
    let binding = StoredRouteBinding {
        client_id: enrolled_client_id,
        server_id: enrolled.server_id,
    };
    save_route_binding(profile_id, &binding)?;
    SecretStore::set(profile_id, ENROLLMENT_URL_KEY, "")?;
    Ok(resolved_identity(&identity, &binding))
}

pub(super) fn decode_signing_key(value: &str) -> AppResult<SigningKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value.trim().as_bytes())
        .map_err(|_| AppError::Message("內建隧道裝置私鑰格式無效。".into()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AppError::Message("內建隧道裝置私鑰長度無效。".into()))?;
    Ok(SigningKey::from_bytes(&bytes))
}

pub(super) fn decode_server_verifying_key(value: &str) -> AppResult<VerifyingKey> {
    let bytes = URL_SAFE_NO_PAD
        .decode(value.trim().as_bytes())
        .map_err(|_| AppError::Message("內建隧道伺服器公鑰格式無效。".into()))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| AppError::Message("內建隧道伺服器公鑰長度無效。".into()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| AppError::Message("內建隧道伺服器公鑰無效。".into()))
}

pub(super) fn parse_enrollment_url(public_url: &str, value: &str) -> AppResult<reqwest::Url> {
    let public = reqwest::Url::parse(public_url)
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    let enrollment = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::Message("一次性註冊連結格式無效。".into()))?;
    if enrollment.scheme() != "https"
        || enrollment.username() != ""
        || enrollment.password().is_some()
        || enrollment.query().is_some()
        || enrollment.fragment().is_some()
        || enrollment.host_str() != public.host_str()
        || enrollment.port_or_known_default() != public.port_or_known_default()
    {
        return Err(AppError::Message(
            "一次性註冊連結必須使用與內建隧道相同的 HTTPS 網域與連接埠。".into(),
        ));
    }
    let prefix = format!("{ENROLL_PATH_PREFIX}/");
    let code = enrollment
        .path()
        .strip_prefix(&prefix)
        .filter(|code| {
            !code.is_empty()
                && code.len() <= 128
                && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
        })
        .ok_or_else(|| {
            AppError::Message("一次性註冊連結必須使用 /_tunnel/enroll/<code> 路徑。".into())
        })?;
    if code.contains('/') {
        return Err(AppError::Message("一次性註冊碼格式無效。".into()));
    }
    Ok(enrollment)
}

fn local_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Coding Tools MCP".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_identity_scope_is_stable_per_origin_and_separates_servers() {
        let a = server_identity_scope_id("https://tunnel.example/builtin/clients/a/mcp").unwrap();
        let b = server_identity_scope_id("https://tunnel.example/builtin/clients/b/mcp").unwrap();
        let c = server_identity_scope_id("https://other.example/builtin/clients/a/mcp").unwrap();
        let d =
            server_identity_scope_id("https://tunnel.example:8443/builtin/clients/a/mcp").unwrap();
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
        assert_eq!(a.len(), 43);
    }

    #[test]
    fn route_binding_is_profile_specific_even_when_device_scope_is_shared() {
        let identity = StoredDeviceIdentity {
            device_id: "device-shared".into(),
            client_id: "legacy-client".into(),
            private_key: "private".into(),
            enrolled: true,
            server_id: "server".into(),
            server_public_key: "public".into(),
        };
        let route_a = StoredRouteBinding {
            client_id: "workspace-a".into(),
            server_id: "server".into(),
        };
        let route_b = StoredRouteBinding {
            client_id: "workspace-b".into(),
            server_id: "server".into(),
        };
        assert_eq!(
            resolved_identity(&identity, &route_a).client_id,
            "workspace-a"
        );
        assert_eq!(
            resolved_identity(&identity, &route_b).client_id,
            "workspace-b"
        );
        assert_eq!(
            resolved_identity(&identity, &route_a).device_id,
            resolved_identity(&identity, &route_b).device_id
        );
    }
}
