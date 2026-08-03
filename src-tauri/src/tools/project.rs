use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use ignore::WalkBuilder;
use serde_json::{json, Value};

use crate::tools::workspace::{relative_display, tool_ok, Workspace, WorkspaceError};

pub fn project_map(ws: &Workspace, args: &Value) -> Result<Value, WorkspaceError> {
    let path = args.get("path").and_then(Value::as_str).unwrap_or(".");
    let resolved = ws.resolve_read_path(path)?;
    if !resolved.path.is_dir() {
        return Err(WorkspaceError::not_a_directory("project_map path must be a directory"));
    }
    let max_files = args
        .get("max_files")
        .and_then(Value::as_u64)
        .unwrap_or(10_000)
        .clamp(1, 50_000) as usize;
    let max_entries = args
        .get("max_entries")
        .and_then(Value::as_u64)
        .unwrap_or(1_000)
        .clamp(1, 10_000) as usize;
    let max_depth = args
        .get("max_depth")
        .and_then(Value::as_u64)
        .unwrap_or(4)
        .clamp(1, 20) as usize;
    let include_hidden = args
        .get("include_hidden")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let include_ignored = args
        .get("include_ignored")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut builder = WalkBuilder::new(&resolved.path);
    builder
        .follow_links(false)
        .hidden(!include_hidden)
        .max_depth(Some(max_depth + 1))
        .require_git(false);
    if include_ignored {
        builder
            .ignore(false)
            .git_ignore(false)
            .git_global(false)
            .git_exclude(false)
            .parents(false);
    }

    let mut scanned_files = 0usize;
    let mut truncated = false;
    let mut manifests = Vec::new();
    let mut entrypoints = BTreeSet::new();
    let mut test_roots = BTreeSet::new();
    let mut language_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut tree = Vec::new();
    let mut package_scripts = BTreeMap::new();

    for item in builder.build().filter_map(Result::ok) {
        let p = item.path();
        if p == resolved.path || !ws.is_safe_read_path(p) {
            continue;
        }
        let rel = relative_display(ws.root(), p);
        if ws.is_ignored_path(p, include_hidden, include_ignored) {
            continue;
        }
        let depth = p.strip_prefix(&resolved.path).map(|v| v.components().count()).unwrap_or(0);
        if tree.len() < max_entries {
            tree.push(json!({
                "path": rel,
                "type": if item.file_type().is_some_and(|t| t.is_dir()) { "directory" } else { "file" },
                "depth": depth
            }));
        }
        if item.file_type().is_some_and(|t| t.is_dir()) {
            if is_test_dir(p) {
                test_roots.insert(rel);
            }
            continue;
        }
        if !item.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        scanned_files += 1;
        if scanned_files > max_files {
            truncated = true;
            break;
        }
        let file_name = p.file_name().and_then(|v| v.to_str()).unwrap_or_default();
        if let Some((kind, language, commands)) = manifest_info(file_name, p) {
            manifests.push(json!({
                "path": rel,
                "kind": kind,
                "language": language,
                "suggested_commands": commands
            }));
            if file_name == "package.json" {
                if let Ok(text) = fs::read_to_string(p) {
                    if let Ok(value) = serde_json::from_str::<Value>(&text) {
                        if let Some(scripts) = value.get("scripts").and_then(Value::as_object) {
                            for (name, command) in scripts {
                                if let Some(command) = command.as_str() {
                                    package_scripts.insert(name.clone(), command.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
        if let Some(language) = language_for_path(p) {
            *language_counts.entry(language.to_string()).or_default() += 1;
        }
        if is_entrypoint(file_name, p) {
            entrypoints.insert(rel);
        }
        if is_test_file(file_name, p) {
            if let Some(parent) = p.parent() {
                test_roots.insert(relative_display(ws.root(), parent));
            }
        }
    }

    tree.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    manifests.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let mut languages = language_counts
        .into_iter()
        .map(|(language, files)| json!({"language": language, "files": files}))
        .collect::<Vec<_>>();
    languages.sort_by(|a, b| {
        b["files"].as_u64().cmp(&a["files"].as_u64())
            .then_with(|| a["language"].as_str().cmp(&b["language"].as_str()))
    });

    let suggested_commands = collect_commands(&manifests, &package_scripts);
    Ok(tool_ok(json!({
        "path": resolved.display,
        "scanned_files": scanned_files.min(max_files),
        "languages": languages,
        "manifests": manifests,
        "entrypoints": entrypoints,
        "test_roots": test_roots,
        "package_scripts": package_scripts,
        "suggested_commands": suggested_commands,
        "tree": tree,
        "truncated": truncated || scanned_files > max_files,
        "tree_truncated": tree.len() >= max_entries,
        "warnings": if truncated { vec!["file scan limit reached"] } else { Vec::<&str>::new() }
    })))
}

fn collect_commands(manifests: &[Value], scripts: &BTreeMap<String, String>) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut commands = Vec::new();
    for manifest in manifests {
        if let Some(items) = manifest.get("suggested_commands").and_then(Value::as_array) {
            for item in items {
                if let Some(command) = item.as_str() {
                    if seen.insert(command.to_string()) {
                        commands.push(json!({"command": command, "source": manifest["path"]}));
                    }
                }
            }
        }
    }
    for name in ["test", "lint", "format", "fmt", "build", "check"] {
        if scripts.contains_key(name) {
            let command = format!("npm run {name}");
            if seen.insert(command.clone()) {
                commands.push(json!({"command": command, "source": "package.json#scripts"}));
            }
        }
    }
    commands
}

fn manifest_info(file_name: &str, path: &Path) -> Option<(&'static str, &'static str, Vec<&'static str>)> {
    match file_name {
        "Cargo.toml" => Some(("cargo", "Rust", vec!["cargo check", "cargo test", "cargo fmt --check"])),
        "package.json" => Some(("npm", "JavaScript/TypeScript", vec!["npm test", "npm run build"])),
        "pyproject.toml" => Some(("pyproject", "Python", vec!["python -m pytest", "python -m compileall ."])),
        "requirements.txt" => Some(("pip", "Python", vec!["python -m pytest"])),
        "setup.py" | "setup.cfg" => Some(("setuptools", "Python", vec!["python -m pytest"])),
        "go.mod" => Some(("go", "Go", vec!["go test ./...", "go vet ./..."])),
        "pom.xml" => Some(("maven", "Java", vec!["mvn test", "mvn package"])),
        "build.gradle" | "build.gradle.kts" => Some(("gradle", "Java/Kotlin", vec!["gradle test", "gradle build"])),
        "composer.json" => Some(("composer", "PHP", vec!["composer test"])),
        "Gemfile" => Some(("bundler", "Ruby", vec!["bundle exec rake test"])),
        "CMakeLists.txt" => Some(("cmake", "C/C++", vec!["cmake --build build"])),
        _ if path.extension().and_then(|v| v.to_str()) == Some("sln") => Some(("dotnet-solution", "C#/.NET", vec!["dotnet build", "dotnet test"])),
        _ if path.extension().and_then(|v| v.to_str()) == Some("csproj") => Some(("dotnet-project", "C#/.NET", vec!["dotnet build", "dotnet test"])),
        _ => None,
    }
}

fn language_for_path(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|v| v.to_str()).unwrap_or_default().to_ascii_lowercase().as_str() {
        "rs" => Some("Rust"), "py" => Some("Python"), "cs" => Some("C#"),
        "js" | "mjs" | "cjs" => Some("JavaScript"), "ts" | "tsx" => Some("TypeScript"),
        "jsx" => Some("JavaScript/JSX"), "java" => Some("Java"), "kt" | "kts" => Some("Kotlin"),
        "go" => Some("Go"), "php" => Some("PHP"), "rb" => Some("Ruby"), "swift" => Some("Swift"),
        "c" | "h" => Some("C"), "cc" | "cpp" | "cxx" | "hpp" => Some("C++"),
        "fs" | "fsx" => Some("F#"), "vb" => Some("Visual Basic"),
        "html" | "htm" => Some("HTML"), "css" | "scss" | "sass" => Some("CSS"),
        "sql" => Some("SQL"), "sh" | "bash" | "zsh" => Some("Shell"), "ps1" => Some("PowerShell"),
        _ => None,
    }
}

fn is_entrypoint(file_name: &str, path: &Path) -> bool {
    matches!(file_name, "main.rs" | "lib.rs" | "main.py" | "app.py" | "manage.py" | "Program.cs" | "Startup.cs" | "index.js" | "index.ts" | "main.js" | "main.ts" | "main.tsx" | "App.tsx")
        || path.components().any(|c| c.as_os_str() == "bin")
}

fn is_test_dir(path: &Path) -> bool {
    path.file_name().and_then(|v| v.to_str()).is_some_and(|name| matches!(name, "test" | "tests" | "spec" | "__tests__"))
}

fn is_test_file(file_name: &str, path: &Path) -> bool {
    file_name.contains(".test.") || file_name.contains(".spec.") || file_name.starts_with("test_") || file_name.ends_with("_test.py") || file_name.ends_with("_test.go") || is_test_dir(path.parent().unwrap_or(Path::new("")))
}
