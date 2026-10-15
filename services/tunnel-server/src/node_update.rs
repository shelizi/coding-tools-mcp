use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::header::{
    CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS,
};
use axum::http::{HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use futures_util::TryStreamExt;
use reqwest::Url;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::{Mutex, RwLock};
use tokio::time::{interval, MissedTickBehavior};

use crate::device_auth::unix_ms;
use crate::observability::Observability;
use crate::AppState;

pub(crate) const LATEST_PATH: &str = "/updates/node-agent/latest";
pub(crate) const DOWNLOAD_PATH: &str =
    "/updates/node-agent/releases/{release_tag}/{edition}/download";

const MANIFEST_NAME: &str = "node-agent-update.json";
const TAG_PREFIX: &str = "node-agent-v";
const PORTABLE_SEP: &str = "-portable-v";
const DEFAULT_POLL_SECONDS: u64 = 300;
const MAX_METADATA_BYTES: usize = 2 * 1024 * 1024;
const MAX_MANIFEST_BYTES: usize = 128 * 1024;

#[derive(Clone)]
pub(crate) struct NodeUpdateCatalog {
    inner: Arc<Inner>,
}

struct Inner {
    config: Option<Config>,
    client: reqwest::Client,
    cache: RwLock<Cache>,
    refresh_lock: Mutex<()>,
}

#[derive(Default)]
struct Cache {
    latest: Option<Release>,
    releases: HashMap<String, Release>,
    checked_at_unix_ms: Option<u64>,
    last_error: Option<String>,
}

#[derive(Clone)]
struct Config {
    base: Url,
    owner: String,
    repo: String,
    token: Option<String>,
    public_origin: String,
    poll_interval: Duration,
}

#[derive(Clone, Debug)]
pub(crate) struct Release {
    pub(crate) tag: String,
    pub(crate) version: String,
    pub(crate) portable_version: String,
    pub(crate) git_commit: String,
    pub(crate) published_at: Option<String>,
    artifacts: HashMap<String, Artifact>,
}

#[derive(Clone, Debug)]
pub(crate) struct Artifact {
    pub(crate) edition: String,
    platform: String,
    arch: String,
    pub(crate) filename: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) signature: String,
    source_url: Url,
}

#[derive(Debug, Deserialize)]
struct GiteaRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GiteaAsset>,
}

#[derive(Clone, Debug, Deserialize)]
struct GiteaAsset {
    name: String,
    #[serde(default)]
    size: u64,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    release_tag: String,
    agent_version: String,
    portable_version: String,
    git_commit: String,
    artifacts: Vec<ManifestArtifact>,
}

#[derive(Debug, Deserialize)]
struct ManifestArtifact {
    edition: String,
    platform: String,
    arch: String,
    filename: String,
    sha256: String,
    signature: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublicInfo {
    pub(crate) schema_version: u32,
    pub(crate) source: String,
    pub(crate) release_tag: String,
    pub(crate) version: String,
    pub(crate) portable_version: String,
    pub(crate) git_commit: String,
    pub(crate) published_at: Option<String>,
    pub(crate) checked_at_unix_ms: Option<u64>,
    pub(crate) artifacts: Vec<PublicArtifact>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct PublicArtifact {
    pub(crate) edition: String,
    pub(crate) platform: String,
    pub(crate) arch: String,
    pub(crate) filename: String,
    pub(crate) size: u64,
    pub(crate) sha256: String,
    pub(crate) signature: String,
    pub(crate) url: String,
}

impl Default for NodeUpdateCatalog {
    fn default() -> Self {
        Self::disabled()
    }
}

impl NodeUpdateCatalog {
    pub(crate) fn disabled() -> Self {
        Self {
            inner: Arc::new(Inner {
                config: None,
                client: http_client().expect("HTTP client"),
                cache: RwLock::new(Cache::default()),
                refresh_lock: Mutex::new(()),
            }),
        }
    }

    pub(crate) fn from_env(public_origin: &str) -> Result<Self, String> {
        let Some(config) = Config::from_env(public_origin)? else {
            return Ok(Self::disabled());
        };
        Ok(Self {
            inner: Arc::new(Inner {
                config: Some(config),
                client: http_client()?,
                cache: RwLock::new(Cache::default()),
                refresh_lock: Mutex::new(()),
            }),
        })
    }

    pub(crate) fn spawn_polling(&self, observability: Observability) {
        let Some(config) = self.inner.config.clone() else {
            return;
        };
        let this = self.clone();
        tokio::spawn(async move {
            if let Err(error) = this.refresh().await {
                tracing::warn!(%error, "initial Gitea Node Agent release refresh failed");
            }
            let mut ticker = interval(config.poll_interval);
            ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
            ticker.tick().await;
            loop {
                ticker.tick().await;
                match this.refresh().await {
                    Ok(release) => observability.log(
                        "info",
                        "update",
                        format!("Node Agent latest release: {}", release.tag),
                        None,
                        Some("mcp"),
                        None,
                        None,
                    ),
                    Err(error) => {
                        tracing::warn!(%error, "Gitea Node Agent release refresh failed")
                    }
                }
            }
        });
    }

    pub(crate) async fn latest_info(&self) -> Result<PublicInfo, String> {
        if self.inner.config.is_none() {
            return Err("Node Agent Gitea updates are not configured".into());
        }
        if self.inner.cache.read().await.latest.is_none() {
            self.refresh().await?;
        }
        let cache = self.inner.cache.read().await;
        let release = cache.latest.as_ref().ok_or_else(|| {
            cache
                .last_error
                .clone()
                .unwrap_or_else(|| "no Node Agent release found".into())
        })?;
        Ok(self.public_info(release, cache.checked_at_unix_ms))
    }

    pub(crate) async fn refresh(&self) -> Result<Release, String> {
        let _guard = self.inner.refresh_lock.lock().await;
        let config = self
            .inner
            .config
            .clone()
            .ok_or_else(|| "Node Agent Gitea updates are not configured".to_string())?;
        let mut url = api_url(&config, &["releases"])?;
        url.query_pairs_mut()
            .append_pair("page", "1")
            .append_pair("limit", "50");
        let releases: Vec<GiteaRelease> = match self.fetch_json(url, MAX_METADATA_BYTES).await {
            Ok(value) => value,
            Err(error) => {
                self.record_error(&error).await;
                return Err(error);
            }
        };
        let gitea_release = match select_latest(releases) {
            Ok(value) => value,
            Err(error) => {
                self.record_error(&error).await;
                return Err(error);
            }
        };
        let release = match self.resolve_gitea_release(&config, gitea_release).await {
            Ok(value) => value,
            Err(error) => {
                self.record_error(&error).await;
                return Err(error);
            }
        };
        let mut cache = self.inner.cache.write().await;
        cache.checked_at_unix_ms = Some(unix_ms());
        cache.last_error = None;
        cache.releases.insert(release.tag.clone(), release.clone());
        cache.latest = Some(release.clone());
        Ok(release)
    }

    async fn record_error(&self, error: &str) {
        let mut cache = self.inner.cache.write().await;
        cache.checked_at_unix_ms = Some(unix_ms());
        cache.last_error = Some(error.to_string());
    }

    async fn resolve_release(&self, tag: &str) -> Result<Release, String> {
        validate_tag(tag)?;
        if let Some(release) = self.inner.cache.read().await.releases.get(tag).cloned() {
            return Ok(release);
        }
        let config = self
            .inner
            .config
            .clone()
            .ok_or_else(|| "Node Agent Gitea updates are not configured".to_string())?;
        let gitea_release: GiteaRelease = self
            .fetch_json(
                api_url(&config, &["releases", "tags", tag])?,
                MAX_METADATA_BYTES,
            )
            .await?;
        if gitea_release.draft || gitea_release.prerelease || gitea_release.tag_name != tag {
            return Err("requested release is not a stable Node Agent release".into());
        }
        let release = self.resolve_gitea_release(&config, gitea_release).await?;
        self.inner
            .cache
            .write()
            .await
            .releases
            .insert(tag.to_string(), release.clone());
        Ok(release)
    }

    async fn resolve_gitea_release(
        &self,
        config: &Config,
        release: GiteaRelease,
    ) -> Result<Release, String> {
        let (tag_agent, tag_portable) = validate_tag(&release.tag_name)?;
        let manifest_asset = release
            .assets
            .iter()
            .find(|asset| asset.name == MANIFEST_NAME)
            .ok_or_else(|| format!("release {} is missing {MANIFEST_NAME}", release.tag_name))?;
        let manifest: Manifest = self
            .fetch_json(
                validate_asset_url(config, &manifest_asset.browser_download_url)?,
                MAX_MANIFEST_BYTES,
            )
            .await?;
        validate_manifest(&release, &manifest, &tag_agent, &tag_portable)?;

        let mut artifacts = HashMap::new();
        for item in &manifest.artifacts {
            let asset = release
                .assets
                .iter()
                .find(|asset| asset.name == item.filename)
                .ok_or_else(|| format!("release is missing {}", item.filename))?;
            if asset.size == 0 {
                return Err(format!("release asset {} has zero size", item.filename));
            }
            artifacts.insert(
                item.edition.clone(),
                Artifact {
                    edition: item.edition.clone(),
                    platform: item.platform.clone(),
                    arch: item.arch.clone(),
                    filename: item.filename.clone(),
                    size: asset.size,
                    sha256: item.sha256.to_ascii_lowercase(),
                    signature: item.signature.clone(),
                    source_url: validate_asset_url(config, &asset.browser_download_url)?,
                },
            );
        }
        Ok(Release {
            tag: release.tag_name,
            version: manifest.agent_version,
            portable_version: manifest.portable_version,
            git_commit: manifest.git_commit,
            published_at: release.published_at,
            artifacts,
        })
    }

    fn public_info(&self, release: &Release, checked_at: Option<u64>) -> PublicInfo {
        let config = self.inner.config.as_ref().expect("enabled config");
        let mut artifacts = release
            .artifacts
            .values()
            .map(|artifact| PublicArtifact {
                edition: artifact.edition.clone(),
                platform: artifact.platform.clone(),
                arch: artifact.arch.clone(),
                filename: artifact.filename.clone(),
                size: artifact.size,
                sha256: artifact.sha256.clone(),
                signature: artifact.signature.clone(),
                url: format!(
                    "{}/updates/node-agent/releases/{}/{}/download",
                    config.public_origin, release.tag, artifact.edition
                ),
            })
            .collect::<Vec<_>>();
        artifacts.sort_by(|a, b| a.edition.cmp(&b.edition));
        PublicInfo {
            schema_version: 1,
            source: "gitea".into(),
            release_tag: release.tag.clone(),
            version: release.version.clone(),
            portable_version: release.portable_version.clone(),
            git_commit: release.git_commit.clone(),
            published_at: release.published_at.clone(),
            checked_at_unix_ms: checked_at,
            artifacts,
        }
    }

    fn request(&self, url: Url) -> reqwest::RequestBuilder {
        let request = self.inner.client.get(url);
        match self
            .inner
            .config
            .as_ref()
            .and_then(|config| config.token.as_ref())
        {
            Some(token) => request.header("Authorization", format!("token {token}")),
            None => request,
        }
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(
        &self,
        url: Url,
        max: usize,
    ) -> Result<T, String> {
        let response = self.request(url).send().await.map_err(http_error)?;
        if !response.status().is_success() {
            return Err(format!("Gitea returned HTTP {}", response.status()));
        }
        if response
            .content_length()
            .is_some_and(|length| length > max as u64)
        {
            return Err("Gitea metadata response is too large".into());
        }
        let bytes = response.bytes().await.map_err(http_error)?;
        if bytes.len() > max {
            return Err("Gitea metadata response is too large".into());
        }
        serde_json::from_slice(&bytes).map_err(|error| format!("invalid Gitea JSON: {error}"))
    }

    async fn artifact_response(&self, tag: &str, edition: &str) -> Result<Response<Body>, String> {
        let release = self.resolve_release(tag).await?;
        let artifact = release
            .artifacts
            .get(edition)
            .cloned()
            .ok_or_else(|| "unknown Node Agent edition".to_string())?;
        let upstream = self
            .request(artifact.source_url.clone())
            .send()
            .await
            .map_err(http_error)?;
        if !upstream.status().is_success() {
            return Err(format!(
                "Gitea artifact returned HTTP {}",
                upstream.status()
            ));
        }
        if upstream
            .content_length()
            .is_some_and(|length| length != artifact.size)
        {
            return Err("Gitea artifact size changed".into());
        }
        let stream = upstream
            .bytes_stream()
            .map_err(|error| std::io::Error::other(error.to_string()));
        let mut response = Response::new(Body::from_stream(stream));
        let headers = response.headers_mut();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/zip"));
        headers.insert(
            CONTENT_LENGTH,
            HeaderValue::from_str(&artifact.size.to_string()).map_err(|_| "invalid size")?,
        );
        headers.insert(
            CONTENT_DISPOSITION,
            HeaderValue::from_str(&format!("attachment; filename=\"{}\"", artifact.filename))
                .map_err(|_| "invalid filename")?,
        );
        headers.insert(
            CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
        headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
        Ok(response)
    }
}

impl Config {
    fn from_env(public_origin: &str) -> Result<Option<Self>, String> {
        let base = std::env::var("CODING_TOOLS_NODE_UPDATE_GITEA_URL").ok();
        let repo = std::env::var("CODING_TOOLS_NODE_UPDATE_GITEA_REPOSITORY").ok();
        if base.is_none() && repo.is_none() {
            return Ok(None);
        }
        let mut base = Url::parse(
            base.as_deref()
                .ok_or("CODING_TOOLS_NODE_UPDATE_GITEA_URL is required")?
                .trim(),
        )
        .map_err(|error| format!("invalid Gitea URL: {error}"))?;
        if base.scheme() != "https"
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
        {
            return Err("Gitea URL must be credential-free HTTPS".into());
        }
        if !base.path().ends_with('/') {
            base.set_path(&format!("{}/", base.path()));
        }
        let (owner, repo) = parse_repository(
            repo.as_deref()
                .ok_or("CODING_TOOLS_NODE_UPDATE_GITEA_REPOSITORY is required")?,
        )?;
        let poll = std::env::var("CODING_TOOLS_NODE_UPDATE_POLL_SECONDS")
            .ok()
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| "invalid poll seconds".to_string())
            })
            .transpose()?
            .unwrap_or(DEFAULT_POLL_SECONDS);
        if poll < 30 {
            return Err("Node update poll interval must be at least 30 seconds".into());
        }
        Ok(Some(Self {
            base,
            owner,
            repo,
            token: read_token()?,
            public_origin: public_origin.trim_end_matches('/').into(),
            poll_interval: Duration::from_secs(poll),
        }))
    }
}

fn read_token() -> Result<Option<String>, String> {
    let direct = std::env::var("CODING_TOOLS_NODE_UPDATE_GITEA_TOKEN").ok();
    let file = std::env::var("CODING_TOOLS_NODE_UPDATE_GITEA_TOKEN_FILE").ok();
    if direct.is_some() && file.is_some() {
        return Err("configure only one Gitea update token source".into());
    }
    let value = match file {
        Some(path) => Some(
            std::fs::read_to_string(path)
                .map_err(|error| format!("failed to read Gitea token file: {error}"))?,
        ),
        None => direct,
    };
    value
        .map(|value| {
            let value = value.trim().to_string();
            if value.is_empty() || value.chars().any(char::is_control) {
                Err("invalid Gitea update token".into())
            } else {
                Ok(value)
            }
        })
        .transpose()
}

fn parse_repository(value: &str) -> Result<(String, String), String> {
    let parts = value.trim().split('/').collect::<Vec<_>>();
    let valid = |part: &&str| {
        !part.is_empty()
            && part.len() <= 128
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    };
    if parts.len() != 2 || !parts.iter().all(valid) {
        return Err("Gitea repository must be owner/repository".into());
    }
    Ok((parts[0].into(), parts[1].into()))
}

fn api_url(config: &Config, suffix: &[&str]) -> Result<Url, String> {
    let mut url = config.base.clone();
    let mut segments = url
        .path_segments_mut()
        .map_err(|_| "invalid hierarchical Gitea URL".to_string())?;
    segments.pop_if_empty();
    segments.extend(["api", "v1", "repos", &config.owner, &config.repo]);
    segments.extend(suffix.iter().copied());
    drop(segments);
    Ok(url)
}

fn validate_asset_url(config: &Config, raw: &str) -> Result<Url, String> {
    let url = Url::parse(raw).map_err(|error| format!("invalid Gitea asset URL: {error}"))?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str() != config.base.host_str()
        || url.port_or_known_default() != config.base.port_or_known_default()
    {
        return Err("Gitea asset URL escaped configured origin".into());
    }
    let base_path = config.base.path().trim_end_matches('/');
    if !base_path.is_empty() && !url.path().starts_with(base_path) {
        return Err("Gitea asset URL escaped configured base path".into());
    }
    Ok(url)
}

fn validate_tag(tag: &str) -> Result<(Version, Version), String> {
    let rest = tag
        .strip_prefix(TAG_PREFIX)
        .ok_or("not a Node Agent release tag")?;
    let (agent, portable) = rest
        .split_once(PORTABLE_SEP)
        .ok_or("invalid Node Agent release tag")?;
    if portable.contains(PORTABLE_SEP) {
        return Err("invalid Node Agent release tag".into());
    }
    Ok((
        Version::parse(agent).map_err(|_| "invalid Node Agent semver")?,
        Version::parse(portable).map_err(|_| "invalid portable semver")?,
    ))
}

fn select_latest(releases: Vec<GiteaRelease>) -> Result<GiteaRelease, String> {
    releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| {
            validate_tag(&release.tag_name)
                .ok()
                .map(|version| (version, release))
        })
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .map(|(_, release)| release)
        .ok_or_else(|| "Gitea has no stable Node Agent portable release".into())
}

fn validate_manifest(
    release: &GiteaRelease,
    manifest: &Manifest,
    agent: &Version,
    portable: &Version,
) -> Result<(), String> {
    if manifest.schema_version != 1
        || manifest.release_tag != release.tag_name
        || manifest.agent_version != agent.to_string()
        || manifest.portable_version != portable.to_string()
    {
        return Err("release manifest identity mismatch".into());
    }
    if manifest.git_commit.len() != 40
        || !manifest
            .git_commit
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("manifest gitCommit must be lowercase 40-hex SHA".into());
    }
    if manifest.artifacts.len() != 2 {
        return Err("manifest must contain exactly two artifacts".into());
    }
    let expected = [
        (
            "bundled-node",
            format!(
                "ctnode-{}-p{}-win64.zip",
                manifest.agent_version, manifest.portable_version
            ),
        ),
        (
            "system-node",
            format!(
                "ctnode-{}-p{}-sys-win64.zip",
                manifest.agent_version, manifest.portable_version
            ),
        ),
    ];
    for (edition, filename) in expected {
        let item = manifest
            .artifacts
            .iter()
            .find(|item| item.edition == edition)
            .ok_or_else(|| format!("manifest missing {edition}"))?;
        if item.platform != "win32" || item.arch != "x64" || item.filename != filename {
            return Err(format!("invalid {edition} artifact identity"));
        }
        if item.sha256.len() != 64 || !item.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid artifact sha256".into());
        }
        let signature = URL_SAFE_NO_PAD
            .decode(&item.signature)
            .map_err(|_| "invalid artifact signature encoding")?;
        if signature.len() != 64 {
            return Err("invalid artifact signature length".into());
        }
    }
    Ok(())
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("coding-tools-tunnel-server/node-update")
        .build()
        .map_err(|error| error.to_string())
}

fn http_error(error: reqwest::Error) -> String {
    if error.is_timeout() {
        "Gitea request timed out".into()
    } else {
        format!("Gitea request failed: {error}")
    }
}

pub(crate) async fn latest(State(state): State<AppState>) -> Response<Body> {
    match state.node_updates.latest_info().await {
        Ok(info) => {
            let mut response = Json(info).into_response();
            response
                .headers_mut()
                .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
            response
        }
        Err(error) => error_response(StatusCode::SERVICE_UNAVAILABLE, error),
    }
}

pub(crate) async fn download(
    State(state): State<AppState>,
    Path((tag, edition)): Path<(String, String)>,
) -> Response<Body> {
    match state.node_updates.artifact_response(&tag, &edition).await {
        Ok(response) => response,
        Err(error) if error == "unknown Node Agent edition" => {
            error_response(StatusCode::NOT_FOUND, error)
        }
        Err(error) => error_response(StatusCode::BAD_GATEWAY, error),
    }
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response<Body> {
    let mut response = (status, Json(json!({"error": message.into()}))).into_response();
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
        .headers_mut()
        .insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, draft: bool, prerelease: bool) -> GiteaRelease {
        GiteaRelease {
            tag_name: tag.into(),
            draft,
            prerelease,
            published_at: None,
            assets: vec![],
        }
    }

    #[test]
    fn latest_filters_other_releases_and_uses_semver() {
        let selected = select_latest(vec![
            release("v9.9.9", false, false),
            release("node-agent-v0.29.35-portable-v2.0.2", false, false),
            release("node-agent-v0.29.36-portable-v2.0.0", true, false),
            release("node-agent-v0.29.35-portable-v2.0.3", false, true),
            release("node-agent-v0.29.34-portable-v9.9.9", false, false),
        ])
        .unwrap();
        assert_eq!(selected.tag_name, "node-agent-v0.29.35-portable-v2.0.2");
    }

    #[test]
    fn repository_and_tag_validation_reject_path_injection() {
        assert_eq!(
            parse_repository("neo/coding-tools-mcp").unwrap(),
            ("neo".into(), "coding-tools-mcp".into())
        );
        assert!(parse_repository("neo/../secret").is_err());
        assert!(validate_tag("node-agent-v0.29.35-portable-v2.0.2").is_ok());
        assert!(validate_tag("v0.29.35").is_err());
    }

    #[test]
    fn manifest_requires_two_signed_windows_x64_artifacts() {
        let tag = "node-agent-v0.29.35-portable-v2.0.2";
        let release = release(tag, false, false);
        let sig = URL_SAFE_NO_PAD.encode([7u8; 64]);
        let manifest = Manifest {
            schema_version: 1,
            release_tag: tag.into(),
            agent_version: "0.29.35".into(),
            portable_version: "2.0.2".into(),
            git_commit: "a".repeat(40),
            artifacts: vec![
                ManifestArtifact {
                    edition: "bundled-node".into(),
                    platform: "win32".into(),
                    arch: "x64".into(),
                    filename: "ctnode-0.29.35-p2.0.2-win64.zip".into(),
                    sha256: "b".repeat(64),
                    signature: sig.clone(),
                },
                ManifestArtifact {
                    edition: "system-node".into(),
                    platform: "win32".into(),
                    arch: "x64".into(),
                    filename: "ctnode-0.29.35-p2.0.2-sys-win64.zip".into(),
                    sha256: "c".repeat(64),
                    signature: sig,
                },
            ],
        };
        let (agent, portable) = validate_tag(tag).unwrap();
        assert!(validate_manifest(&release, &manifest, &agent, &portable).is_ok());
    }
}
