use coding_tools_tunnel_protocol::{is_hop_by_hop_header, HeaderPair, TunnelService};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, HOST};

use super::BuiltinTunnelConfig;

pub(super) struct IncomingRequest {
    pub(super) request_id: String,
    pub(super) method: String,
    pub(super) path_and_query: String,
    pub(super) headers: Vec<HeaderPair>,
    pub(super) body: Vec<u8>,
}

pub(super) fn prepare_local_request(
    config: &BuiltinTunnelConfig,
    http: &reqwest::Client,
    request: IncomingRequest,
) -> Result<(String, reqwest::RequestBuilder), String> {
    let request_id = request.request_id.clone();
    if !request.path_and_query.starts_with('/') || request.path_and_query.starts_with("//") {
        return Err("server supplied an invalid relative request path".into());
    }
    let local_path = local_path_for_request(config, &request.path_and_query)?;
    let url = format!("{}{}", config.local_base_url, local_path);
    let method = reqwest::Method::from_bytes(request.method.as_bytes())
        .map_err(|error| error.to_string())?;
    let mut builder = http.request(method, url).body(request.body);
    for header in request.headers {
        if is_hop_by_hop_header(&header.name) || header.name.eq_ignore_ascii_case(HOST.as_str()) {
            continue;
        }
        let Ok(name) = HeaderName::from_bytes(header.name.as_bytes()) else {
            continue;
        };
        let Ok(value) = HeaderValue::from_str(&header.value) else {
            continue;
        };
        builder = builder.header(name, value);
    }
    Ok((request_id, builder))
}

pub(super) fn response_headers(headers: &HeaderMap) -> Vec<HeaderPair> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            if is_hop_by_hop_header(name.as_str()) {
                return None;
            }
            value.to_str().ok().map(|value| HeaderPair {
                name: name.as_str().to_string(),
                value: value.to_string(),
            })
        })
        .collect()
}

pub(super) fn local_path_for_request(
    config: &BuiltinTunnelConfig,
    path_and_query: &str,
) -> Result<String, String> {
    if config.service == TunnelService::Mcp {
        return Ok(path_and_query.to_string());
    }
    let (path, query) = path_and_query
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((path_and_query, None));
    let suffix = path
        .strip_prefix(&config.route_prefix)
        .ok_or_else(|| "Actions request does not match registered route".to_string())?;
    if !suffix.is_empty() && !suffix.starts_with('/') {
        return Err("Actions route prefix matched a partial segment".into());
    }
    let local = if suffix.is_empty() { "/" } else { suffix };
    Ok(match query {
        Some(query) => format!("{local}?{query}"),
        None => local.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_headers_remove_hop_by_hop_values_and_invalid_text() {
        let mut headers = HeaderMap::new();
        headers.insert("connection", HeaderValue::from_static("keep-alive"));
        headers.insert("x-tunnel-test", HeaderValue::from_static("visible"));
        headers.insert(
            "x-binary",
            HeaderValue::from_bytes(&[0xFF]).expect("binary header value"),
        );

        assert_eq!(
            response_headers(&headers),
            vec![HeaderPair {
                name: "x-tunnel-test".into(),
                value: "visible".into(),
            }]
        );
    }
}
