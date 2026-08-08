use std::path::PathBuf;
use std::process::Command;

fn git_output(manifest_dir: &PathBuf, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(manifest_dir)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default());
    let build_git_sha =
        git_output(&manifest_dir, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=CTMCP_BUILD_GIT_SHA={build_git_sha}");

    if let Some(git_head_path) = git_output(&manifest_dir, &["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={git_head_path}");
    }
    if let Some(head_ref) = git_output(&manifest_dir, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(ref_path) = git_output(&manifest_dir, &["rev-parse", "--git-path", &head_ref]) {
            println!("cargo:rerun-if-changed={ref_path}");
        }
    }

    #[cfg(feature = "desktop")]
    tauri_build::build();
}
