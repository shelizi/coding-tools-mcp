use coding_tools_tunnel_protocol::{valid_client_id, TunnelService, WS_PATH};

use crate::error::{AppError, AppResult};

pub(super) struct ParsedBuiltinEndpoint {
    pub(super) public_url: String,
    pub(super) websocket_url: String,
    pub(super) client_id: String,
    pub(super) route_prefix: String,
}

pub(super) fn builtin_endpoint_for_client(
    value: &str,
    service: TunnelService,
    client_id: &str,
) -> AppResult<ParsedBuiltinEndpoint> {
    if !valid_client_id(client_id) {
        return Err(AppError::Message(
            "內建隧道 Client ID 只能包含英文字母、數字、- 與 _。".into(),
        ));
    }
    let parsed = parse_builtin_endpoint(value, service)?;
    let mut url = reqwest::Url::parse(&parsed.public_url)
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    let path = match service {
        TunnelService::Mcp => format!("/builtin/clients/{client_id}/mcp"),
        TunnelService::Actions => format!("/builtin/actions/{client_id}"),
    };
    url.set_path(&path);
    parse_builtin_endpoint(url.as_str(), service)
}

pub(super) fn parse_builtin_endpoint(
    value: &str,
    service: TunnelService,
) -> AppResult<ParsedBuiltinEndpoint> {
    let mut url = reqwest::Url::parse(value.trim())
        .map_err(|_| AppError::Message("內建隧道公開網址格式無效。".into()))?;
    if url.scheme() != "https" {
        return Err(AppError::Message("內建隧道公開網址必須使用 HTTPS。".into()));
    }
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(AppError::Message(
            "內建隧道公開網址不得包含帳號、密碼、query 或 fragment。".into(),
        ));
    }
    let segments = url
        .path_segments()
        .map(|segments| segments.filter(|part| !part.is_empty()).collect::<Vec<_>>())
        .unwrap_or_default();
    let (client_id, route_prefix) = match service {
        TunnelService::Mcp
            if (segments.len() == 3 || segments.len() == 4)
                && segments[0] == "builtin"
                && segments[1] == "clients"
                && (segments.len() == 3 || segments[3] == "mcp") =>
        {
            (
                segments[2].to_string(),
                format!("/builtin/clients/{}", segments[2]),
            )
        }
        TunnelService::Actions
            if segments.len() == 3 && segments[0] == "builtin" && segments[1] == "actions" =>
        {
            (
                segments[2].to_string(),
                format!("/builtin/actions/{}", segments[2]),
            )
        }
        TunnelService::Mcp => {
            return Err(AppError::Message(
                "內建 MCP 網址必須使用 /builtin/clients/<client-id>/mcp。".into(),
            ));
        }
        TunnelService::Actions => {
            return Err(AppError::Message(
                "內建 Actions 網址必須使用 /builtin/actions/<client-id>。".into(),
            ));
        }
    };
    if !valid_client_id(&client_id) {
        return Err(AppError::Message(
            "內建隧道 Client ID 只能包含英文字母、數字、- 與 _。".into(),
        ));
    }

    let public_path = match service {
        TunnelService::Mcp => format!("{route_prefix}/mcp"),
        TunnelService::Actions => route_prefix.clone(),
    };
    url.set_path(&public_path);
    let public_url = url.as_str().trim_end_matches('/').to_string();
    url.set_scheme("wss")
        .map_err(|_| AppError::Message("無法建立內建 WSS 網址。".into()))?;
    url.set_path(WS_PATH);
    let websocket_url = url.to_string();

    Ok(ParsedBuiltinEndpoint {
        public_url,
        websocket_url,
        client_id,
        route_prefix,
    })
}
