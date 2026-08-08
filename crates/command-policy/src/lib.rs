pub const DEFAULT_PROCESS_TIMEOUT_MS: u64 = 30_000;

pub fn package_script_looks_long_running(display: &str) -> bool {
    let lower = display.to_ascii_lowercase();
    let package_manager = [
        "npm ",
        "npm.cmd ",
        "pnpm ",
        "pnpm.cmd ",
        "yarn ",
        "yarn.cmd ",
        "bun ",
        "bun.exe ",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !package_manager || !lower.contains(" run ") {
        return false;
    }
    lower.split_whitespace().any(|token| {
        [
            "build", "portable", "package", "release", "verify", "test", "check", "sync", "parity",
        ]
        .iter()
        .any(|keyword| token.contains(keyword))
    })
}

pub fn command_looks_long_running(command_kind: &str, display: &str) -> bool {
    let lower = display.to_ascii_lowercase();
    matches!(command_kind, "cargo_test" | "cargo_check" | "build")
        || lower.contains("cargo clippy")
        || lower.contains("tauri build")
        || package_script_looks_long_running(display)
}

pub fn resolved_command_timeout_ms(
    explicit_timeout_ms: Option<u64>,
    command_kind: &str,
    display: &str,
    configured_max_ms: u64,
    absolute_max_ms: u64,
) -> u64 {
    let absolute_max_ms = absolute_max_ms.max(1);
    if let Some(explicit) = explicit_timeout_ms {
        return explicit.clamp(1, absolute_max_ms);
    }
    if command_looks_long_running(command_kind, display) {
        configured_max_ms.clamp(1, absolute_max_ms)
    } else {
        DEFAULT_PROCESS_TIMEOUT_MS.min(absolute_max_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIGURED_MAX: u64 = 30 * 60_000;
    const ABSOLUTE_MAX: u64 = 60 * 60_000;

    #[test]
    fn known_build_and_packaging_commands_get_long_default_timeout() {
        for (kind, display) in [
            ("build", "cmd.exe /d /s /c npm run desktop:portable"),
            ("other", "npm --prefix packages/node-agent run build:server"),
            ("other", "npm run node-agent:parity:check"),
            (
                "other",
                "npm --prefix packages/node-agent run sync:rust-contract",
            ),
            (
                "cargo_test",
                "cargo test --manifest-path src-tauri/Cargo.toml",
            ),
            ("other", "cargo clippy --manifest-path src-tauri/Cargo.toml"),
            ("other", "tauri build --features custom-protocol"),
        ] {
            assert_eq!(
                resolved_command_timeout_ms(None, kind, display, CONFIGURED_MAX, ABSOLUTE_MAX),
                CONFIGURED_MAX,
                "{display}"
            );
        }
    }

    #[test]
    fn ordinary_and_explicit_timeouts_keep_their_contract() {
        assert_eq!(
            resolved_command_timeout_ms(
                None,
                "other",
                "node scripts/check.mjs",
                CONFIGURED_MAX,
                ABSOLUTE_MAX
            ),
            DEFAULT_PROCESS_TIMEOUT_MS
        );
        assert_eq!(
            resolved_command_timeout_ms(
                Some(600_000),
                "build",
                "npm run desktop:portable",
                CONFIGURED_MAX,
                ABSOLUTE_MAX
            ),
            600_000
        );
        assert_eq!(
            resolved_command_timeout_ms(
                Some(ABSOLUTE_MAX + 1),
                "other",
                "echo ok",
                CONFIGURED_MAX,
                ABSOLUTE_MAX
            ),
            ABSOLUTE_MAX
        );
    }
}
