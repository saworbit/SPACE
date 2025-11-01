use anyhow::Result;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use clap::{Parser, Subcommand};
use common::CapsuleId;
use nvram_sim::NvramLog;
use std::fs;

#[derive(Parser)]
#[command(name = "spacectl")]
#[command(about = "SPACE storage control utility", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create a new capsule from data
    Create {
        /// Input file path
        #[arg(short, long)]
        file: String,
    },
    /// Read capsule contents
    Read {
        /// Capsule UUID
        capsule_id: String,
    },
    /// List all capsules
    List,
    /// Start S3-compatible HTTP server
    ServeS3 {
        /// Port to listen on
        #[arg(short, long, default_value = "8080")]
        port: u16,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize tracing only for serve-s3 command
    if matches!(cli.command, Commands::ServeS3 { .. }) {
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .init();
    }

    match cli.command {
        Commands::Create { file } => {
            let registry = CapsuleRegistry::new();
            let nvram = NvramLog::open("space.nvram")?;
            let pipeline = WritePipeline::new(registry, nvram);

            let data = fs::read(&file)?;
            let id = pipeline.write_capsule(&data)?;
            println!("✅ Capsule created: {}", id.as_uuid());
            println!("   Size: {} bytes", data.len());
        }
        Commands::Read { capsule_id } => {
            let registry = CapsuleRegistry::new();
            let nvram = NvramLog::open("space.nvram")?;
            let pipeline = WritePipeline::new(registry, nvram);

            let uuid = capsule_id.parse()?;
            let id = CapsuleId::from_uuid(uuid);
            let data = pipeline.read_capsule(id)?;
            std::io::Write::write_all(&mut std::io::stdout(), &data)?;
        }
        Commands::List => {
            println!("📦 Capsules:");
            // TODO: implement list in registry
            println!("(list not yet implemented)");
        }
        Commands::ServeS3 { port } => {
            use protocol_s3::{server::S3Server, S3View};

            println!("🚀 Starting SPACE S3 Protocol View...");

            let registry = CapsuleRegistry::new();
            let nvram = NvramLog::open("space.nvram")?;
            let s3_view = S3View::new(registry, nvram);

            let server = S3Server::new(s3_view, port);

            // Create a new tokio runtime for the async server
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async { server.run().await })?;
        }
    }

    Ok(())
}
