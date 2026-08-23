use std::time::Duration;

use crate::error::{AppError, AppResult};
use crate::settings::AppSettings;

const MAX_RELEASE_ASSET_BYTES: u64 = 128 * 1024 * 1024;

/// The official GitHub URL plus an optional user-configured mirror fallback.
/// Callers always try `primary` first, then `fallback`.
pub struct DownloadPlan {
    pub primary: String,
    pub fallback: Option<String>,
}

/// Build the ordered list of URLs to attempt for a GitHub release asset.
///
/// The official GitHub URL is always primary. A mirror is used only as an
/// explicit fallback; downloaded executables must still be integrity checked.
pub fn plan_github_download(settings: &AppSettings, github_url: &str) -> DownloadPlan {
    let prefix = settings.download.github_mirror.trim();
    if prefix.is_empty() {
        return DownloadPlan {
            primary: github_url.to_string(),
            fallback: None,
        };
    }
    let mirrored = format!(
        "{}/{}",
        prefix.trim_end_matches('/'),
        github_url.trim_start_matches('/')
    );
    DownloadPlan {
        primary: github_url.to_string(),
        fallback: Some(mirrored),
    }
}

/// Build a reqwest client honoring the configured proxy mode.
///
/// - `none`: no proxy (default direct connection)
/// - `system`: reqwest's built-in system-proxy detection
/// - anything else: treated as an explicit proxy URL (http/https/socks5)
fn build_client(settings: &AppSettings) -> AppResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder().timeout(Duration::from_secs(180));
    let mode = settings.download.proxy_mode.trim();
    match mode {
        "" | "none" => {
            builder = builder.no_proxy();
        }
        "system" => {
            // Leave reqwest's default system-proxy detection enabled.
        }
        url => {
            let proxy = reqwest::Proxy::all(url)
                .map_err(|err| AppError::Message(format!("代理地址无效: {err}")))?;
            builder = builder.proxy(proxy);
        }
    }
    builder
        .build()
        .map_err(|err| AppError::Message(err.to_string()))
}

/// Download bytes from a GitHub release asset, honoring mirror + proxy config.
///
/// Tries the official GitHub URL first, then an explicitly configured mirror.
/// `label` is used in error messages (e.g. "frpc").
pub async fn download_release_asset(
    settings: &AppSettings,
    github_url: &str,
    label: &str,
) -> AppResult<Vec<u8>> {
    let plan = plan_github_download(settings, github_url);
    let client = build_client(settings)?;

    let mut urls = vec![plan.primary];
    if let Some(fallback) = plan.fallback {
        urls.push(fallback);
    }

    let mut last_err = String::new();
    for url in urls {
        match fetch_bytes(&client, &url).await {
            Ok(bytes) => return Ok(bytes),
            Err(err) => {
                last_err = err;
            }
        }
    }
    Err(AppError::Message(format!("下载 {label} 失败: {last_err}")))
}

async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|err| err.to_string())?
        .error_for_status()
        .map_err(|err| err.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RELEASE_ASSET_BYTES)
    {
        return Err(format!(
            "release asset exceeds {} MiB limit",
            MAX_RELEASE_ASSET_BYTES / (1024 * 1024)
        ));
    }

    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|err| err.to_string())? {
        let next_len = bytes.len().saturating_add(chunk.len()) as u64;
        if next_len > MAX_RELEASE_ASSET_BYTES {
            return Err(format!(
                "release asset exceeds {} MiB limit",
                MAX_RELEASE_ASSET_BYTES / (1024 * 1024)
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::plan_github_download;
    use crate::settings::AppSettings;

    #[test]
    fn official_github_is_primary_even_with_a_mirror() {
        let mut settings = AppSettings::default();
        settings.download.github_mirror = "https://mirror.example".into();
        let official = "https://github.com/fatedier/frp/releases/download/v1/file.zip";

        let plan = plan_github_download(&settings, official);

        assert_eq!(plan.primary, official);
        assert_eq!(
            plan.fallback.as_deref(),
            Some("https://mirror.example/https://github.com/fatedier/frp/releases/download/v1/file.zip")
        );
    }

    #[test]
    fn default_download_has_no_third_party_fallback() {
        let settings = AppSettings::default();
        let official = "https://github.com/fatedier/frp/releases/download/v1/file.zip";

        let plan = plan_github_download(&settings, official);

        assert_eq!(plan.primary, official);
        assert!(plan.fallback.is_none());
    }
}
