mod tx;

use clap::Parser;
use color_eyre::eyre::{eyre, Result};
use metrics::{backend::map::MapMetricsBackend, types::MetricsData, MetricsService};
use nomos_http::backends::axum::AxumBackend;
use nomos_http::bridge::{HttpBridgeService, HttpBridgeSettings};
use nomos_http::http::HttpService;
use nomos_log::Logger;
use nomos_mempool::backend::mockpool::MockPool;
use nomos_mempool::network::adapters::waku::WakuAdapter;
use nomos_mempool::MempoolService;
use nomos_network::{backends::waku::Waku, NetworkService};
use overwatch_derive::*;
use overwatch_rs::{
    overwatch::*,
    services::{handle::ServiceHandle, ServiceData},
};
use serde::Deserialize;
use tx::{Tx, TxId};
/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path for a yaml-encoded network config file
    config: std::path::PathBuf,
}

#[derive(Deserialize)]
struct Config {
    log: <Logger as ServiceData>::Settings,
    network: <NetworkService<Waku> as ServiceData>::Settings,
    metrics: <MetricsService<MapMetricsBackend<MetricsData>> as ServiceData>::Settings,
    http: <HttpService<AxumBackend> as ServiceData>::Settings,
}

#[derive(Services)]
struct MockPoolNode {
    logging: ServiceHandle<Logger>,
    network: ServiceHandle<NetworkService<Waku>>,
    metrics: ServiceHandle<MetricsService<MapMetricsBackend<MetricsData>>>,
    mockpool: ServiceHandle<MempoolService<WakuAdapter<Tx>, MockPool<TxId, Tx>>>,
    http: ServiceHandle<HttpService<AxumBackend>>,
    bridges: ServiceHandle<HttpBridgeService>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let Args { config } = Args::parse();
    let config = serde_yaml::from_reader::<_, Config>(std::fs::File::open(config)?)?;
    let bridges = Vec::new();
    let app = OverwatchRunner::<MockPoolNode>::run(
        MockPoolNodeServiceSettings {
            network: config.network,
            logging: config.log,
            metrics: config.metrics,
            http: config.http,
            mockpool: (),
            bridges: HttpBridgeSettings { bridges },
        },
        None,
    )
    .map_err(|e| eyre!("Error encountered: {}", e))?;
    app.wait_finished();
    Ok(())
}
