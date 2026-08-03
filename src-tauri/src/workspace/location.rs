use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionTarget {
    Host,
    Wsl { distro: String, linux_path: String },
}

impl Default for ExecutionTarget {
    fn default() -> Self {
        Self::Host
    }
}

impl ExecutionTarget {
    pub fn from_host_path(path: &str) -> Self {
        parse_wsl_unc_path(path)
            .map(|location| Self::Wsl {
                distro: location.distro,
                linux_path: location.linux_path,
            })
            .unwrap_or(Self::Host)
    }

    pub fn normalize_for_host_path(&mut self, path: &str) -> Option<String> {
        let parsed = parse_wsl_unc_path(path);
        match self {
            Self::Host => {
                let location = parsed?;
                let host_path = location.host_path();
                *self = Self::Wsl {
                    distro: location.distro,
                    linux_path: location.linux_path,
                };
                Some(host_path)
            }
            Self::Wsl { distro, linux_path } => {
                if let Some(location) = parsed {
                    *distro = location.distro;
                    *linux_path = location.linux_path;
                } else {
                    *distro = distro.trim().to_string();
                    *linux_path = normalize_linux_path(linux_path);
                }
                Some(wsl_unc_path(distro, linux_path))
            }
        }
    }

    pub fn wsl_location(&self) -> Option<WslLocation> {
        match self {
            Self::Host => None,
            Self::Wsl { distro, linux_path } => Some(WslLocation {
                distro: distro.clone(),
                linux_path: normalize_linux_path(linux_path),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslLocation {
    pub distro: String,
    pub linux_path: String,
}

impl WslLocation {
    pub fn host_path(&self) -> String {
        wsl_unc_path(&self.distro, &self.linux_path)
    }
}

pub fn parse_wsl_unc_path(path: impl AsRef<str>) -> Option<WslLocation> {
    let normalized = path.as_ref().trim().replace('/', "\\");
    let body = normalized
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| normalized.strip_prefix(r"\\?\unc\"))
        .or_else(|| normalized.strip_prefix(r"\\"))?;
    let mut parts = body.splitn(3, '\\');
    let server = parts.next()?;
    if !server.eq_ignore_ascii_case("wsl.localhost") && !server.eq_ignore_ascii_case("wsl$") {
        return None;
    }
    let distro = parts.next()?.trim();
    if distro.is_empty() || distro.contains(['/', '\\', '\0']) {
        return None;
    }
    let remainder = parts.next().unwrap_or_default().replace('\\', "/");
    Some(WslLocation {
        distro: distro.to_string(),
        linux_path: normalize_linux_path(&format!("/{remainder}")),
    })
}

pub fn parse_wsl_path(path: &Path) -> Option<WslLocation> {
    parse_wsl_unc_path(path.to_string_lossy())
}

/// Compare WSL workspace paths using Windows semantics for the share and
/// distribution name, while preserving Linux case sensitivity below it.
/// Returns `None` when neither path is a WSL path so callers can retain their
/// platform-native host path comparison behavior.
pub fn compare_wsl_paths(left: impl AsRef<str>, right: impl AsRef<str>) -> Option<bool> {
    match (
        parse_wsl_unc_path(left.as_ref()),
        parse_wsl_unc_path(right.as_ref()),
    ) {
        (None, None) => None,
        (Some(left), Some(right)) => Some(
            left.distro.eq_ignore_ascii_case(&right.distro) && left.linux_path == right.linux_path,
        ),
        _ => Some(false),
    }
}

pub fn wsl_unc_path(distro: &str, linux_path: &str) -> String {
    let distro = distro.trim();
    let linux_path = normalize_linux_path(linux_path);
    if linux_path == "/" {
        format!(r"\\wsl.localhost\{distro}")
    } else {
        format!(
            r"\\wsl.localhost\{}\{}",
            distro,
            linux_path.trim_start_matches('/').replace('/', "\\")
        )
    }
}

fn normalize_linux_path(path: &str) -> String {
    let replaced = path.trim().replace('\\', "/");
    let segments = replaced
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect::<Vec<_>>();
    if segments.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", segments.join("/"))
    }
}

#[cfg(test)]
mod tests {
    use super::{compare_wsl_paths, parse_wsl_unc_path, wsl_unc_path, ExecutionTarget};

    #[test]
    fn parses_supported_wsl_unc_forms() {
        for path in [
            r"\\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject",
            r"\\wsl$\Ubuntu-24.04\opt\src\SampleProject",
            r"\\?\UNC\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject",
        ] {
            let parsed = parse_wsl_unc_path(path).expect("WSL path");
            assert_eq!(parsed.distro, "Ubuntu-24.04");
            assert_eq!(parsed.linux_path, "/opt/src/SampleProject");
        }
    }

    #[test]
    fn normalizes_wsl_unc_to_localhost_form() {
        assert_eq!(
            wsl_unc_path("Ubuntu-24.04", "/opt/src/SampleProject/"),
            r"\\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject"
        );
    }

    #[test]
    fn normalizes_redundant_linux_path_segments() {
        let parsed = parse_wsl_unc_path(r"\\wsl.localhost\Ubuntu-24.04\opt\\src\.\SampleProject\\")
            .expect("WSL path");
        assert_eq!(parsed.linux_path, "/opt/src/SampleProject");
        assert_eq!(
            wsl_unc_path("Ubuntu-24.04", "/opt//src/./SampleProject/"),
            r"\\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject"
        );
        assert_eq!(
            compare_wsl_paths(
                r"\\wsl.localhost\Ubuntu-24.04\opt\\src\.\SampleProject",
                r"\\wsl$\ubuntu-24.04\opt\src\SampleProject",
            ),
            Some(true)
        );
    }

    #[test]
    fn execution_target_is_inferred_from_legacy_path() {
        assert_eq!(
            ExecutionTarget::from_host_path(r"\\wsl$\Ubuntu\home\dev"),
            ExecutionTarget::Wsl {
                distro: "Ubuntu".into(),
                linux_path: "/home/dev".into()
            }
        );
    }

    #[test]
    fn compares_linux_segments_with_case_sensitive_semantics() {
        assert_eq!(
            compare_wsl_paths(
                r"\\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject",
                r"\\wsl$\ubuntu-24.04\opt\src\SampleProject",
            ),
            Some(true)
        );
        assert_eq!(
            compare_wsl_paths(
                r"\\wsl.localhost\Ubuntu-24.04\opt\src\SampleProject",
                r"\\wsl.localhost\Ubuntu-24.04\opt\src\sampleproject",
            ),
            Some(false)
        );
        assert_eq!(compare_wsl_paths(r"C:\src\Demo", r"c:\src\demo"), None);
    }
}
