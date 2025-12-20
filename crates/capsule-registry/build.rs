fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=PROTOC");

    let should_set_vendored = match std::env::var_os("PROTOC") {
        None => true,
        Some(path) => !std::path::Path::new(&path).is_file(),
    };

    if should_set_vendored {
        let protoc = protoc_bin_vendored::protoc_bin_path()?;
        println!("cargo:warning=Using vendored protoc at {}", protoc.display());
        std::env::set_var("PROTOC", protoc);
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile(&["proto/registry_raft.proto"], &["proto"])?;
    Ok(())
}
