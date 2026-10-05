// Build script:
// - compiles the gRPC `.proto` definitions (server + client stubs) and emits
//   the encoded file descriptor set used by gRPC server reflection;
// - records the compiler version for `/identity` (RUSTYBIN_RUSTC_VERSION);
// - records the commit for `/_rustybin/version` (RUSTYBIN_GIT_SHA);
// - embeds the web console (`ui/`).
//
// We point tonic-build at a vendored `protoc` binary so the build needs no
// system protobuf compiler - important for the slim Docker image and clean
// `cargo build` on any developer machine.
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    tonic_build::configure()
        .build_server(true)
        .build_client(true) // client stubs are used by the tests
        .file_descriptor_set_path(out_dir.join("echo_descriptor.bin"))
        .compile_protos(&["proto/echo.proto"], &["proto"])?;

    // `rustc --version` of the compiler building this crate.
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let version = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RUSTYBIN_RUSTC_VERSION={version}");

    if let Some(sha) = git_sha() {
        println!("cargo:rustc-env=RUSTYBIN_GIT_SHA={sha}");
    }

    embed_ui(&out_dir)?;

    println!("cargo:rerun-if-changed=proto/echo.proto");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");
    println!("cargo:rerun-if-env-changed=RUSTYBIN_GIT_SHA");
    Ok(())
}

/// The commit being built, for `/_rustybin/version` and the metrics build
/// info: `RUSTYBIN_GIT_SHA` when set (Docker builds have no `.git`), else the
/// checkout's HEAD (12 hex digits). Also asks Cargo to rerun the script when
/// HEAD moves.
fn git_sha() -> Option<String> {
    if let Ok(sha) = std::env::var("RUSTYBIN_GIT_SHA") {
        let sha = sha.trim();
        if !sha.is_empty() && sha.len() <= 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(sha.chars().take(12).collect());
        }
        return None;
    }
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let sha = git(&["rev-parse", "--short=12", "HEAD"])?;
    // Rerun when HEAD or the current branch ref changes (only for paths that
    // exist: a missing path would make Cargo rerun the script every build).
    let mut watch = Vec::new();
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        watch.push(PathBuf::from(&dir).join("HEAD"));
    }
    if let Some(common) = git(&["rev-parse", "--git-common-dir"]) {
        let common = PathBuf::from(common);
        let common = if common.is_absolute() {
            common
        } else {
            std::env::current_dir()
                .map(|d| d.join(&common))
                .unwrap_or(common)
        };
        watch.push(common.join("packed-refs"));
        if let Some(head_ref) = git(&["symbolic-ref", "-q", "HEAD"]) {
            watch.push(common.join(head_ref));
        }
    }
    for path in watch.into_iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    sha.bytes().all(|b| b.is_ascii_hexdigit()).then_some(sha)
}

/// Generate `$OUT_DIR/ui_assets.rs`: a table of every file under `ui/`
/// (relative path, bytes) embedded with `include_bytes!`, served by `src/ui.rs`.
fn embed_ui(out_dir: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let path = entry?.path();
            let hidden = path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with('.'));
            if hidden {
                continue;
            }
            if path.is_dir() {
                walk(&path, out)?;
            } else {
                out.push(path);
            }
        }
        Ok(())
    }
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("ui");
    let mut files = Vec::new();
    if root.is_dir() {
        walk(&root, &mut files)?;
    }
    files.sort();
    let mut code = String::from(
        "/// Embedded console files: (path relative to `ui/`, contents).\n\
         pub static UI_ASSETS: &[(&str, &[u8])] = &[\n",
    );
    for file in &files {
        let rel = file
            .strip_prefix(&root)?
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        code.push_str(&format!(
            "    ({rel:?}, include_bytes!({:?})),\n",
            file.to_string_lossy()
        ));
    }
    code.push_str("];\n");
    std::fs::write(out_dir.join("ui_assets.rs"), code)?;
    println!("cargo:rerun-if-changed=ui");
    Ok(())
}
