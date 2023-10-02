use crate::da::{DaProtocolChoice, DisseminateApp, DisseminateAppServiceSettings, Settings};
use clap::Args;
use nomos_network::{backends::libp2p::Libp2p, NetworkService};
use overwatch_rs::{overwatch::OverwatchRunner, services::ServiceData};
use reqwest::Url;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;

const NODE_CERT_PATH: &str = "mempool-da/add";

#[derive(Args, Debug)]
pub struct Disseminate {
    // TODO: accept bytes
    #[clap(short, long)]
    pub data: String,
    /// Path to the network config file
    #[clap(short, long)]
    pub network_config: PathBuf,
    /// The data availability protocol to use. Defaults to full replication.
    #[clap(flatten)]
    pub da_protocol: DaProtocolChoice,
    /// Timeout in seconds. Defaults to 120 seconds.
    #[clap(short, long, default_value = "120")]
    pub timeout: u64,
    /// Address of the node to send the certificate to
    /// for block inclusion, if present.
    #[clap(long)]
    pub node_addr: Option<Url>,
    /// File to write the certificate to, if present.
    #[clap(long)]
    pub output: Option<PathBuf>,
    // Whether to wait for the certificate to be included in a block
    #[clap(long, default_value = "false")]
    pub wait_for_inclusion: bool,
}

impl Disseminate {
    pub fn run(&self) -> Result<(), Box<dyn std::error::Error>> {
        tracing::subscriber::set_global_default(tracing_subscriber::FmtSubscriber::new())
            .expect("setting tracing default failed");
        let network = serde_yaml::from_reader::<
            _,
            <NetworkService<Libp2p> as ServiceData>::Settings,
        >(std::fs::File::open(&self.network_config)?)?;
        let (status_updates, rx) = std::sync::mpsc::channel();
        let bytes: Box<[u8]> = self.data.clone().as_bytes().into();
        let timeout = Duration::from_secs(self.timeout);
        let da_protocol = self.da_protocol.clone();
        let node_addr = self.node_addr.clone();
        let output = self.output.clone();
        let (payload_sender, payload_rx) = tokio::sync::mpsc::unbounded_channel();
        payload_sender.send(bytes).unwrap();
        let wait_for_inclusion = self.wait_for_inclusion;
        std::thread::spawn(move || {
            OverwatchRunner::<DisseminateApp>::run(
                DisseminateAppServiceSettings {
                    network,
                    send_blob: Settings {
                        payload: Arc::new(Mutex::new(payload_rx)),
                        timeout,
                        da_protocol,
                        status_updates,
                        node_addr,
                        output,
                        wait_for_inclusion,
                    },
                },
                None,
            )
            .unwrap()
            .wait_finished();
        });

        tracing::info!("{}", rx.recv().unwrap().display());
        while let Ok(update) = rx.recv() {
            tracing::info!("{}", update.display());
        }
        Ok(())
    }
}
