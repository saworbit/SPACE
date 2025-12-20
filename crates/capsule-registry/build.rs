fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("PROTOC").is_none() {
        let protoc = protoc_bin_vendored::protoc_bin_path()?;
        std::env::set_var("PROTOC", protoc);
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile(&["proto/registry_raft.proto"], &["proto"])?;
    Ok(())
}
