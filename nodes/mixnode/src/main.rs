use clap::Parser;
use color_eyre::eyre::Result;
use mixnet_node::{MixnetNode, MixnetNodeConfig};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path for a yaml-encoded mixnet-node config file
    config: std::path::PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install global collector configured based on RUST_LOG env var.
    // TODO: use the log service that nomos-node uses, if necessary
    tracing_subscriber::fmt::init();

    let Args { config } = Args::parse();
    let config = serde_yaml::from_reader::<_, MixnetNodeConfig>(std::fs::File::open(config)?)?;

    let node = MixnetNode::new(config);

    //TODO: graceful shutdown
    if let Err(e) = node.run().await {
        tracing::error!("error from mixnet-node: {e}");
    }

    Ok(())
}
