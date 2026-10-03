// Compile the gRPC `.proto` definitions at build time.
//
// We point tonic-build at a vendored `protoc` binary so the build needs no
// system protobuf compiler - important for the slim Docker image and clean
// `cargo build` on any developer machine.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_build::configure()
        .build_server(true)
        .build_client(true) // client stubs are used by the integration tests
        .compile_protos(&["proto/echo.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/echo.proto");
    Ok(())
}
