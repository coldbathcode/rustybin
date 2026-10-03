// Build script:
// - compiles the gRPC `.proto` definitions (server + client stubs) and emits
//   the encoded file descriptor set used by gRPC server reflection;
// - records the compiler version for `/identity` (RUSTYBIN_RUSTC_VERSION).
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

    println!("cargo:rerun-if-changed=proto/echo.proto");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RUSTC");
    Ok(())
}
