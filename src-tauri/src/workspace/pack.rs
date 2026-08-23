use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::canonical::{
    desktop_profile_from_canonical, encode_stored_profile, parse_canonical_workspace,
    CanonicalWorkspace,
};
use super::WorkspaceProfile;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SecretPresence {
    #[serde(default)]
    pub oauth_password: bool,
    #[serde(default)]
    pub oauth_client_secret: bool,
    #[serde(default)]
    pub oauth_token_secret: bool,
    #[serde(default)]
    pub tunnel_enrollment_url: bool,
}

pub fn secret_presence_from_map(secrets: Option<&HashMap<String, String>>) -> SecretPresence {
    let get = |key: &str| {
        secrets
            .and_then(|map| map.get(key))
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
    };
    SecretPresence {
        oauth_password: get("oauth_password"),
        oauth_client_secret: get("oauth_client_secret"),
        oauth_token_secret: get("oauth_token_secret"),
        tunnel_enrollment_url: get("builtin_tunnel_enrollment_url"),
    }
}

fn strip_machine_local(value: &mut Value) {
    if let Some(node) = value
        .pointer_mut("/host/node")
        .and_then(Value::as_object_mut)
    {
        node.remove("dataDir");
    }
}

pub fn build_workspace_pack(
    profile: &WorkspaceProfile,
    presence: SecretPresence,
) -> Result<Value, String> {
    let mut value = encode_stored_profile(profile)?;
    strip_machine_local(&mut value);
    if let Some(object) = value.as_object_mut() {
        object.insert(
            "secretPresence".into(),
            serde_json::to_value(presence).map_err(|error| error.to_string())?,
        );
    }
    Ok(value)
}

pub fn parse_workspace_pack(value: &Value) -> Result<(CanonicalWorkspace, SecretPresence), String> {
    let mut pack = value.clone();
    let presence = pack
        .as_object_mut()
        .and_then(|object| object.remove("secretPresence"))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    strip_machine_local(&mut pack);
    Ok((parse_canonical_workspace(&pack)?, presence))
}

pub fn local_app_data_dir() -> PathBuf {
    if let Ok(path) = std::env::var("LOCALAPPDATA") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    if let Ok(path) = std::env::var("XDG_DATA_HOME") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".local")
        .join("share")
}

pub fn shared_workspaces_root() -> PathBuf {
    if let Ok(path) = std::env::var("CTMCP_SHARED_WORKSPACES_ROOT") {
        if !path.trim().is_empty() {
            return PathBuf::from(path);
        }
    }
    local_app_data_dir()
        .join("CodingToolsMCP")
        .join("workspaces")
}

pub fn shared_workspace_file(id: &str) -> PathBuf {
    shared_workspaces_root().join(id).join("workspace.json")
}

pub fn shared_secrets_file(id: &str) -> PathBuf {
    shared_workspaces_root().join(id).join("secrets.json")
}

pub fn shared_secrets_file_next_to(workspace_file: &Path) -> PathBuf {
    workspace_file.with_file_name("secrets.json")
}

pub fn profile_from_workspace_pack(pack: &Value) -> AppResult<WorkspaceProfile> {
    let (document, _) = parse_workspace_pack(pack).map_err(AppError::Message)?;
    for folder in &document.folders {
        if !Path::new(&folder.path).is_dir() {
            return Err(AppError::Message(format!(
                "workspace folder is missing: {}",
                folder.path
            )));
        }
    }
    Ok(desktop_profile_from_canonical(&document))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn presence() -> SecretPresence {
        SecretPresence {
            oauth_password: true,
            oauth_token_secret: true,
            oauth_client_secret: false,
            tunnel_enrollment_url: false,
        }
    }

    #[test]
    fn export_omits_secrets_and_node_data_dir() {
        let root = tempdir().expect("tempdir");
        let mut profile = WorkspaceProfile::new(
            root.path().to_string_lossy().into_owned(),
            Some("pack".into()),
        );
        profile.runtime.local_port = 18790;
        let mut pack = build_workspace_pack(&profile, presence()).expect("pack");
        let text = pack.to_string();
        assert!(!text.contains("\"password\""));
        assert!(!text.contains("tokenSecret"));
        assert!(!text.contains("clientSecret"));
        assert!(!text.contains("enrollmentUrl"));
        assert!(!text.contains("cloudflareToken"));
        assert_eq!(pack["secretPresence"]["oauthPassword"], true);
        assert_eq!(pack["secretPresence"]["oauthTokenSecret"], true);
        assert!(pack.pointer("/host/node/dataDir").is_none());
        if let Some(node) = pack
            .pointer_mut("/host/node")
            .and_then(Value::as_object_mut)
        {
            node.insert("dataDir".into(), Value::String("C:/leak".into()));
        }
        let (document, parsed_presence) = parse_workspace_pack(&pack).expect("parse");
        assert!(parsed_presence.oauth_password);
        assert!(document
            .host
            .node
            .as_object()
            .and_then(|node| node.get("dataDir"))
            .is_none());
    }

    #[test]
    fn import_rejects_missing_folder() {
        let profile =
            WorkspaceProfile::new("C:/missing-pack-folder-ctmcp".into(), Some("gone".into()));
        let pack = build_workspace_pack(&profile, SecretPresence::default()).expect("pack");
        let error = profile_from_workspace_pack(&pack).expect_err("missing folder");
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn shared_workspace_file_uses_codingtools_drop_layout() {
        let previous = std::env::var("CTMCP_SHARED_WORKSPACES_ROOT").ok();
        std::env::set_var(
            "CTMCP_SHARED_WORKSPACES_ROOT",
            "C:/tmp/CodingToolsMCP/workspaces",
        );
        let path = shared_workspace_file("ws-drop");
        assert_eq!(
            shared_secrets_file("ws-drop"),
            shared_secrets_file_next_to(&path)
        );
        assert!(path.ends_with(std::path::Path::new("ws-drop").join("workspace.json")));
        assert!(path
            .to_string_lossy()
            .replace('\\', "/")
            .contains("CodingToolsMCP/workspaces/ws-drop/workspace.json"));
        match previous {
            Some(value) => std::env::set_var("CTMCP_SHARED_WORKSPACES_ROOT", value),
            None => std::env::remove_var("CTMCP_SHARED_WORKSPACES_ROOT"),
        }
    }
}
