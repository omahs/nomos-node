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
    // Construct a subscriber that prints formatted traces to stdout
    // and use that subscriber to process traces emitted after this point
    // TODO: use the log service that nomos-node uses, if necessary
    let subscriber = tracing_subscriber::FmtSubscriber::new();
    tracing::subscriber::set_global_default(subscriber)?;

    let Args { config } = Args::parse();
    let config = serde_yaml::from_reader::<_, MixnetNodeConfig>(std::fs::File::open(config)?)?;

    let node = MixnetNode::new(config);

    match node.run().await {
        Ok(handle) => {
            tokio::pin!(handle);
            loop {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {
                        tracing::info!("received shutdown signal, gracefully shutdown");
                        handle.shutdown();
                        return Ok(());
                    }
                    _ = &mut handle => {
                        return Ok(());
                    }
                }
            }
        }
        Err(e) => {
            tracing::error!("error from mixnet-node: {e}");
        }
    }

    Ok(())
}
