mod client;

use crate::settings::AppSettings;
#[allow(unused_imports)]
use crate::settings::FrpProfile;
use crate::workspace::{parse_mcp_public_endpoint, McpPublicEndpoint, WorkspaceProfile};
use std::collections::HashSet;

use super::TunnelServiceKind;

pub(crate) use client::{
    acquire_frpc_operation_lock, clear_managed_frpc_pid, managed_frpc_config_matches,
    stop_recorded_frpc_instance,
};
pub(crate) use client::{cached_frpc_path, download_frpc_to_cache};
pub use client::{resolve_frpc, spawn_frpc};

const FRP_VERSION: &str = "0.61.0";
pub(crate) const VERSION: &str = FRP_VERSION;

#[allow(dead_code)]
pub(crate) fn frp_version() -> &'static str {
    FRP_VERSION
}

/// FRP proxy snippet for the MCP listener (`profile.tunnel` + `profile.runtime`).
#[allow(dead_code)]
pub fn mcp_frp_snippet(profile: &WorkspaceProfile, settings: &AppSettings) -> String {
    frp_snippet(profile, TunnelServiceKind::Mcp, settings)
}

/// FRP proxy snippet for the Actions listener (`profile.actions`).
#[allow(dead_code)]
pub fn actions_frp_snippet(profile: &WorkspaceProfile, settings: &AppSettings) -> String {
    frp_snippet(profile, TunnelServiceKind::Actions, settings)
}

pub fn frp_snippet(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> String {
    let config = frp_server_config(profile, kind, settings, None);
    build_proxy_snippet(&config.proxy)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) enum FrpProxyRoute {
    Subdomain(String),
    DomainPaths {
        domain: String,
        locations: Vec<String>,
    },
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FrpProxyConfig {
    pub proxy_name: String,
    pub local_ip: String,
    pub local_port: u16,
    pub route: FrpProxyRoute,
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct FrpServerConfig {
    pub server_addr: String,
    pub server_port: u16,
    pub token: Option<String>,
    pub transport_protocol: String,
    pub proxy: FrpProxyConfig,
}

#[allow(dead_code)]
pub fn frp_public_url(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> String {
    if kind == TunnelServiceKind::Mcp {
        if let Ok(endpoint) = parse_mcp_public_endpoint(&profile.tunnel.public_url) {
            return endpoint.endpoint_url;
        }
    }

    let config = frp_server_config(profile, kind, settings, None);
    match &config.proxy.route {
        FrpProxyRoute::Subdomain(subdomain)
            if !config.server_addr.trim().is_empty() && !subdomain.trim().is_empty() =>
        {
            format!(
                "https://{}.{}",
                subdomain.trim(),
                config.server_addr.trim().trim_end_matches('/')
            )
        }
        _ => String::new(),
    }
}

pub fn frp_server_config(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
    token_override: Option<String>,
) -> FrpServerConfig {
    let endpoint = if kind == TunnelServiceKind::Mcp {
        parse_mcp_public_endpoint(&profile.tunnel.public_url).ok()
    } else {
        None
    };
    let (profile_id, server_addr, server_port) = match kind {
        TunnelServiceKind::Mcp => (
            profile.tunnel.frp_profile_id.as_str(),
            profile.tunnel.frp_server.clone(),
            profile.tunnel.frp_server_port,
        ),
        TunnelServiceKind::Actions => (
            profile.actions.frp_profile_id.as_str(),
            profile.actions.frp_server.clone(),
            profile.actions.frp_server_port,
        ),
    };
    let matching_profile = endpoint.as_ref().and_then(|endpoint| {
        settings.frp_profiles.iter().find(|profile| {
            normalized_server_host(&profile.server).eq_ignore_ascii_case(&endpoint.server_host)
        })
    });
    let profile_id = if profile_id.trim().is_empty() {
        matching_profile
            .map(|profile| profile.id.as_str())
            .unwrap_or(profile_id)
    } else {
        profile_id
    };
    let (server_addr, server_port, transport_protocol) = if let Some(endpoint) = endpoint.as_ref() {
        (
            endpoint.server_host.clone(),
            endpoint.server_port,
            "wss".to_string(),
        )
    } else if let Some(frp_profile) = settings.find_frp_profile(profile_id) {
        (
            frp_profile.server.clone(),
            frp_profile.server_port,
            "tcp".to_string(),
        )
    } else {
        (server_addr, server_port, "tcp".to_string())
    };
    let proxy = frp_proxy_config(profile, kind, endpoint.as_ref());

    let token = token_override.or_else(|| resolve_frp_token(profile_id, profile, kind, settings));

    FrpServerConfig {
        server_addr,
        server_port,
        token,
        transport_protocol,
        proxy,
    }
}

fn resolve_frp_token(
    profile_id: &str,
    workspace: &WorkspaceProfile,
    kind: TunnelServiceKind,
    settings: &AppSettings,
) -> Option<String> {
    if !profile_id.trim().is_empty() {
        if let Ok(Some(token)) =
            crate::secret::SecretStore::get_app("frp_profile_token", profile_id)
        {
            if !token.trim().is_empty() {
                return Some(token);
            }
        }
    }

    let workspace_key = match kind {
        TunnelServiceKind::Mcp => "frp_token",
        TunnelServiceKind::Actions => "actions_frp_token",
    };
    if let Ok(Some(token)) = crate::secret::SecretStore::get(&workspace.id, workspace_key) {
        if !token.trim().is_empty() {
            return Some(token);
        }
    }

    // Manual inline server: reuse token from a global profile with the same host.
    let inline_server = match kind {
        TunnelServiceKind::Mcp => workspace.tunnel.frp_server.as_str(),
        TunnelServiceKind::Actions => workspace.actions.frp_server.as_str(),
    };
    let inline_server = inline_server.trim();
    if !inline_server.is_empty() {
        for profile in &settings.frp_profiles {
            if profile.server.trim().eq_ignore_ascii_case(inline_server) {
                if let Ok(Some(token)) =
                    crate::secret::SecretStore::get_app("frp_profile_token", &profile.id)
                {
                    if !token.trim().is_empty() {
                        return Some(token);
                    }
                }
            }
        }
    }

    None
}

#[allow(dead_code)]
fn normalized_server_host(value: &str) -> String {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() {
        return String::new();
    }
    if let Ok(url) = reqwest::Url::parse(value) {
        if let Some(host) = url.host_str() {
            return host.to_ascii_lowercase();
        }
    }
    value
        .split_once(':')
        .map(|(host, _)| host)
        .unwrap_or(value)
        .trim_matches('[')
        .trim_matches(']')
        .to_ascii_lowercase()
}

#[allow(dead_code)]
pub fn build_frpc_toml(config: &FrpServerConfig) -> String {
    let mut lines = vec![
        format!("serverAddr = \"{}\"", config.server_addr.trim()),
        format!("serverPort = {}", config.server_port),
        format!("transport.protocol = \"{}\"", config.transport_protocol),
        String::new(),
    ];
    if let Some(token) = config.token.as_ref().filter(|t| !t.trim().is_empty()) {
        lines.push("auth.method = \"token\"".to_string());
        lines.push(format!("auth.token = \"{}\"", token.trim()));
        lines.push(String::new());
    }
    lines.push(build_proxy_snippet(&config.proxy));
    lines.join("\n")
}

/// Build one frpc configuration containing all active proxies.
///
/// A single frpc process can serve multiple workspaces, but all proxies must
/// share the same server connection. The supervisor validates that invariant
/// before calling this function.
pub(crate) fn build_frpc_toml_for_routes(configs: &[FrpServerConfig]) -> String {
    let Some(first) = configs.first() else {
        return String::new();
    };

    let mut lines = vec![
        format!("serverAddr = \"{}\"", first.server_addr.trim()),
        format!("serverPort = {}", first.server_port),
        format!("transport.protocol = \"{}\"", first.transport_protocol),
        String::new(),
    ];
    if let Some(token) = first.token.as_ref().filter(|t| !t.trim().is_empty()) {
        lines.push("auth.method = \"token\"".to_string());
        lines.push(format!("auth.token = \"{}\"", token.trim()));
        lines.push(String::new());
    }

    let mut used_names = HashSet::new();
    for config in configs {
        let mut proxy = config.proxy.clone();
        let base_name = proxy.proxy_name.clone();
        let mut name = base_name.clone();
        let mut suffix = 2;
        while !used_names.insert(name.clone()) {
            name = format!("{base_name}-{suffix}");
            suffix += 1;
        }
        proxy.proxy_name = name;
        lines.push(build_proxy_snippet(&proxy));
        lines.push(String::new());
    }

    lines.pop();
    lines.join("\n")
}

pub(crate) fn build_frpc_toml_for_route_refs(
    routes: &[(&WorkspaceProfile, TunnelServiceKind)],
    settings: &AppSettings,
) -> String {
    let configs: Vec<FrpServerConfig> = routes
        .iter()
        .map(|(profile, kind)| frp_server_config(profile, *kind, settings, None))
        .collect();
    build_frpc_toml_for_routes(&configs)
}

fn frp_proxy_config(
    profile: &WorkspaceProfile,
    kind: TunnelServiceKind,
    endpoint: Option<&McpPublicEndpoint>,
) -> FrpProxyConfig {
    let prefix = workspace_proxy_prefix(&profile.id);
    match kind {
        TunnelServiceKind::Mcp => FrpProxyConfig {
            proxy_name: format!("{prefix}-mcp"),
            local_ip: crate::workspace::connect_address_for_bind(&profile.runtime.bind_address),
            local_port: profile.runtime.local_port,
            route: endpoint
                .map(|endpoint| FrpProxyRoute::DomainPaths {
                    domain: endpoint.server_host.clone(),
                    locations: endpoint.frp_locations(),
                })
                .unwrap_or_else(|| FrpProxyRoute::Subdomain(profile.tunnel.frp_subdomain.clone())),
        },
        TunnelServiceKind::Actions => FrpProxyConfig {
            proxy_name: format!("{prefix}-actions"),
            local_ip: crate::workspace::connect_address_for_bind(&profile.actions.bind_address),
            local_port: profile.actions.local_port,
            route: FrpProxyRoute::Subdomain(profile.actions.frp_subdomain.clone()),
        },
    }
}

fn build_proxy_snippet(proxy: &FrpProxyConfig) -> String {
    let mut lines = vec![
        "[[proxies]]".to_string(),
        format!("name = \"{}\"", proxy.proxy_name),
        "type = \"http\"".to_string(),
        format!("localIP = \"{}\"", proxy.local_ip),
        format!("localPort = {}", proxy.local_port),
    ];
    match &proxy.route {
        FrpProxyRoute::Subdomain(subdomain) => {
            lines.push(format!("subdomain = \"{}\"", subdomain.trim()));
        }
        FrpProxyRoute::DomainPaths { domain, locations } => {
            lines.push(format!("customDomains = [\"{}\"]", domain.trim()));
            let locations = locations
                .iter()
                .map(|location| format!("\"{}\"", location.trim()))
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!("locations = [{locations}]"));
        }
    }
    lines.join("\n")
}

fn workspace_proxy_prefix(workspace_id: &str) -> String {
    let stable_id: String = workspace_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    if stable_id.is_empty() {
        "workspace".to_string()
    } else {
        format!("ws-{}", stable_id.to_ascii_lowercase())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::FrpProfile;
    use crate::workspace::WorkspaceProfile;

    #[test]
    fn mcp_snippet_uses_tunnel_subdomain() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo WS".into()));
        profile.tunnel.tunnel_type = "frp".into();
        profile.tunnel.public_url.clear();
        profile.tunnel.frp_subdomain = "demo-mcp".into();
        profile.runtime.local_port = 28766;
        profile.runtime.bind_address = "0.0.0.0".into();
        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "Main".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        profile.tunnel.frp_profile_id = "p1".into();

        let snippet = mcp_frp_snippet(&profile, &settings);
        let proxy_name = frp_server_config(&profile, TunnelServiceKind::Mcp, &settings, None)
            .proxy
            .proxy_name;
        assert!(snippet.contains(&format!("name = \"{proxy_name}\"")));
        assert!(snippet.contains("localIP = \"127.0.0.1\""));
        assert!(snippet.contains("localPort = 28766"));
        assert!(snippet.contains("subdomain = \"demo-mcp\""));
    }

    #[test]
    fn frp_uses_specific_listener_address() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo".into()));
        profile.runtime.bind_address = "192.168.1.20".into();
        profile.tunnel.frp_subdomain = "demo".into();

        let snippet = mcp_frp_snippet(&profile, &AppSettings::default());

        assert!(snippet.contains("localIP = \"192.168.1.20\""));
    }

    #[test]
    fn build_frpc_toml_uses_global_profile_server() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo".into()));
        profile.tunnel.tunnel_type = "frp".into();
        profile.tunnel.public_url.clear();
        profile.tunnel.frp_subdomain = "demo".into();
        profile.tunnel.frp_profile_id = "p1".into();
        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "Main".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        let config = frp_server_config(
            &profile,
            TunnelServiceKind::Mcp,
            &settings,
            Some("secret".into()),
        );
        let toml = build_frpc_toml(&config);
        assert!(toml.contains("serverAddr = \"frp.example.com\""));
        assert!(toml.contains("auth.token = \"secret\""));
    }

    #[test]
    fn public_mcp_url_generates_wss_domain_and_location() {
        let mut profile =
            WorkspaceProfile::new("/tmp/url-client".into(), Some("URL Client".into()));
        profile.tunnel.public_url = "https://coding-tools.example.com/clients/pc-a/mcp".into();
        profile.runtime.local_port = 28_766;

        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "matching-host".into(),
                name: "Matching Host".into(),
                server: "coding-tools.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        let config = frp_server_config(&profile, TunnelServiceKind::Mcp, &settings, None);
        let toml = build_frpc_toml(&config);

        assert_eq!(config.server_addr, "coding-tools.example.com");
        assert_eq!(config.server_port, 443);
        assert_eq!(config.transport_protocol, "wss");
        assert!(matches!(            config.proxy.route,
            FrpProxyRoute::DomainPaths { ref domain, ref locations }
                if domain == "coding-tools.example.com"
                    && locations == &vec![
                        "/clients/pc-a".to_string(),
                        "/.well-known/oauth-authorization-server/clients/pc-a".to_string(),
                        "/.well-known/oauth-protected-resource/clients/pc-a/mcp".to_string(),
                    ]
        ));
        assert!(toml.contains("transport.protocol = \"wss\""));
        assert!(toml.contains("customDomains = [\"coding-tools.example.com\"]"));
        assert!(toml.contains(
            "locations = [\"/clients/pc-a\", \"/.well-known/oauth-authorization-server/clients/pc-a\", \"/.well-known/oauth-protected-resource/clients/pc-a/mcp\"]"
        ));
        assert!(!toml.contains("subdomain ="));
        assert_eq!(
            frp_public_url(&profile, TunnelServiceKind::Mcp, &settings),
            "https://coding-tools.example.com/clients/pc-a/mcp"
        );
    }

    #[test]
    fn public_mcp_url_honors_explicit_https_port() {
        let mut profile = WorkspaceProfile::new("/tmp/url-port".into(), None);
        profile.tunnel.public_url = "https://mcp.example.com:8443/clients/a/mcp".into();

        let config = frp_server_config(
            &profile,
            TunnelServiceKind::Mcp,
            &AppSettings::default(),
            None,
        );

        assert_eq!(config.server_addr, "mcp.example.com");
        assert_eq!(config.server_port, 8443);
        assert_eq!(config.transport_protocol, "wss");
    }

    #[test]
    fn build_frpc_toml_for_routes_contains_all_proxies() {
        let mut first = WorkspaceProfile::new("/tmp/first".into(), Some("First".into()));
        first.tunnel.tunnel_type = "frp".into();
        first.tunnel.public_url.clear();
        first.tunnel.frp_server = "frp.example.com".into();
        first.tunnel.frp_server_port = 7000;
        first.tunnel.frp_subdomain = "first".into();
        first.runtime.local_port = 28766;

        let mut second = WorkspaceProfile::new("/tmp/second".into(), Some("Second".into()));
        second.tunnel.tunnel_type = "frp".into();
        second.tunnel.public_url.clear();
        second.tunnel.frp_server = "frp.example.com".into();
        second.tunnel.frp_server_port = 7000;
        second.tunnel.frp_subdomain = "second".into();
        second.runtime.local_port = 28767;

        let settings = AppSettings::default();
        let configs = vec![
            frp_server_config(&first, TunnelServiceKind::Mcp, &settings, None),
            frp_server_config(&second, TunnelServiceKind::Mcp, &settings, None),
        ];
        let first_name = configs[0].proxy.proxy_name.clone();
        let second_name = configs[1].proxy.proxy_name.clone();
        let toml = build_frpc_toml_for_routes(&configs);

        assert_eq!(toml.matches("[[proxies]]").count(), 2);
        assert!(toml.contains("serverAddr = \"frp.example.com\""));
        assert!(toml.contains(&format!("name = \"{first_name}\"")));
        assert!(toml.contains(&format!("name = \"{second_name}\"")));
        assert!(toml.contains("localPort = 28766"));
        assert!(toml.contains("localPort = 28767"));
    }

    #[test]
    fn build_frpc_toml_for_routes_supports_mcp_and_actions_together() {
        let mut mcp = WorkspaceProfile::new("/tmp/mcp".into(), Some("MCP".into()));
        mcp.tunnel.frp_server = "frp.example.com".into();
        mcp.tunnel.frp_server_port = 7000;
        mcp.tunnel.frp_subdomain = "mcp".into();
        mcp.runtime.local_port = 28766;

        let mut actions = WorkspaceProfile::new("/tmp/actions".into(), Some("Actions".into()));
        actions.actions.frp_server = "frp.example.com".into();
        actions.actions.frp_server_port = 7000;
        actions.actions.frp_subdomain = "actions".into();
        actions.actions.local_port = 8787;

        let settings = AppSettings::default();
        let configs = vec![
            frp_server_config(&mcp, TunnelServiceKind::Mcp, &settings, None),
            frp_server_config(&actions, TunnelServiceKind::Actions, &settings, None),
        ];
        let mcp_name = configs[0].proxy.proxy_name.clone();
        let actions_name = configs[1].proxy.proxy_name.clone();
        let toml = build_frpc_toml_for_routes(&configs);

        assert_eq!(toml.matches("[[proxies]]").count(), 2);
        assert!(toml.contains(&format!("name = \"{mcp_name}\"")));
        assert!(toml.contains(&format!("name = \"{actions_name}\"")));
        assert!(toml.contains("localPort = 28766"));
        assert!(toml.contains("localPort = 8787"));
    }

    #[test]
    fn build_frpc_toml_for_routes_keeps_workspace_proxy_names_unique() {
        let mut first = WorkspaceProfile::new("/tmp/first".into(), Some("Same Name".into()));
        first.tunnel.frp_server = "frp.example.com".into();
        first.tunnel.frp_server_port = 7000;
        first.tunnel.frp_subdomain = "first".into();

        let mut second = WorkspaceProfile::new("/tmp/second".into(), Some("Same Name".into()));
        second.tunnel.frp_server = "frp.example.com".into();
        second.tunnel.frp_server_port = 7000;
        second.tunnel.frp_subdomain = "second".into();

        let settings = AppSettings::default();
        let configs = vec![
            frp_server_config(&first, TunnelServiceKind::Mcp, &settings, None),
            frp_server_config(&second, TunnelServiceKind::Mcp, &settings, None),
        ];
        let first_name = configs[0].proxy.proxy_name.clone();
        let second_name = configs[1].proxy.proxy_name.clone();
        let toml = build_frpc_toml_for_routes(&configs);

        assert_ne!(first_name, second_name);
        assert!(toml.contains(&format!("name = \"{first_name}\"")));
        assert!(toml.contains(&format!("name = \"{second_name}\"")));
    }

    #[test]
    fn build_frpc_toml_for_routes_returns_empty_for_no_routes() {
        assert!(build_frpc_toml_for_routes(&[]).is_empty());
    }

    #[test]
    fn same_name_workspaces_receive_distinct_proxy_names() {
        let first = WorkspaceProfile::new("/tmp/first".into(), Some("Same Name".into()));
        let second = WorkspaceProfile::new("/tmp/second".into(), Some("Same Name".into()));
        let settings = AppSettings::default();

        let first_config = frp_server_config(&first, TunnelServiceKind::Mcp, &settings, None);
        let second_config = frp_server_config(&second, TunnelServiceKind::Mcp, &settings, None);

        assert_ne!(
            first_config.proxy.proxy_name,
            second_config.proxy.proxy_name
        );
    }

    #[test]
    fn proxy_name_is_stable_when_workspace_is_renamed() {
        let original = WorkspaceProfile::new("/tmp/demo".into(), Some("Before".into()));
        let mut renamed = original.clone();
        renamed.name = "After".into();
        let settings = AppSettings::default();

        let before = frp_server_config(&original, TunnelServiceKind::Mcp, &settings, None);
        let after = frp_server_config(&renamed, TunnelServiceKind::Mcp, &settings, None);

        assert_eq!(before.proxy.proxy_name, after.proxy.proxy_name);
    }

    #[test]
    fn resolve_token_from_matching_global_profile_when_manual_server() {
        let mut profile = WorkspaceProfile::new("/tmp/demo".into(), Some("Demo".into()));
        profile.tunnel.frp_server = "frp.example.com".into();
        profile.tunnel.frp_subdomain = "demo".into();
        let settings = AppSettings {
            frp_profiles: vec![FrpProfile {
                id: "p1".into(),
                name: "Main".into(),
                server: "frp.example.com".into(),
                server_port: 7000,
            }],
            ..AppSettings::default()
        };
        crate::secret::SecretStore::set_app("frp_profile_token", "p1", "shared-token").unwrap();
        let config = frp_server_config(&profile, TunnelServiceKind::Mcp, &settings, None);
        assert_eq!(config.token.as_deref(), Some("shared-token"));
    }
}
