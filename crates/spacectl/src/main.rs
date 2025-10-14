use anyhow::Result;
use capsule_registry::{CapsuleRegistry, pipeline::WritePipeline};
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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    let registry = CapsuleRegistry::new();
    let nvram = NvramLog::open("space.nvram")?;
    let pipeline = WritePipeline::new(registry, nvram);
    
    match cli.command {
        Commands::Create { file } => {
            let data = fs::read(&file)?;
            let id = pipeline.write_capsule(&data)?;
            println!("✅ Capsule created: {}", id.as_uuid());
            println!("   Size: {} bytes", data.len());
        }
        Commands::Read { capsule_id } => {
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
    }
    
    Ok(())
}