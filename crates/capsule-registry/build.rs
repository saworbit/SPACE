fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=PROTOC");

    let configured_protoc = std::env::var_os("PROTOC").map(std::path::PathBuf::from);

    let mut use_vendored = configured_protoc.is_none();

    if let Some(path) = &configured_protoc {
        let output = std::process::Command::new(path).arg("--version").output();
        match output {
            Ok(out) if out.status.success() => {
                use_vendored = false;
            }
            _ => {
                use_vendored = true;
            }
        }
    }

    if use_vendored {
        let protoc = protoc_bin_vendored::protoc_bin_path()?;
        println!(
            "cargo:warning=Using vendored protoc at {}",
            protoc.display()
        );
        std::env::set_var("PROTOC", protoc);
    } else if let Some(path) = configured_protoc {
        println!("cargo:warning=Using protoc from PROTOC={}", path.display());
    }

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile(&["proto/registry_raft.proto"], &["proto"])?;
    Ok(())
}
