use clap::{Args, ValueEnum};
use full_replication::{AbsoluteNumber, FullReplication};
use futures::StreamExt;
use nomos_core::da::{blob::Blob, DaProtocol};
use nomos_da::network::{adapters::libp2p::Libp2pAdapter, NetworkAdapter};
use nomos_network::{backends::libp2p::Libp2p, NetworkService};
use overwatch_derive::*;
use overwatch_rs::{
    services::{
        handle::{ServiceHandle, ServiceStateHandle},
        relay::NoMessage,
        state::*,
        ServiceCore, ServiceData, ServiceId,
    },
    DynError,
};
use reqwest::Url;
use serde::Serialize;
use std::{
    path::PathBuf,
    sync::{mpsc::Sender, Arc},
    time::Duration,
};
use tokio::sync::{mpsc::UnboundedReceiver, Mutex};

pub async fn disseminate_and_wait<D, B, N, A, C>(
    mut da: D,
    data: Box<[u8]>,
    adapter: N,
    status_updates: Sender<Status>,
    node_addr: Option<&Url>,
    output: Option<&PathBuf>,
    wait_for_inclusion: bool,
) -> Result<(), Box<dyn std::error::Error>>
where
    D: DaProtocol<Blob = B, Attestation = A, Certificate = C>,
    N: NetworkAdapter<Blob = B, Attestation = A> + Send + Sync,
    C: Serialize,
{
    // 1) Building blob
    status_updates.send(Status::Encoding)?;
    let blobs = da.encode(data);

    // 2) Send blob to network
    status_updates.send(Status::Disseminating)?;
    futures::future::try_join_all(blobs.into_iter().map(|blob| adapter.send_blob(blob)))
        .await
        .map_err(|e| e as Box<dyn std::error::Error>)?;

    // 3) Collect attestations and create proof
    status_updates.send(Status::WaitingAttestations)?;
    let mut attestations = adapter.attestation_stream().await;
    let cert: C = loop {
        da.recv_attestation(attestations.next().await.unwrap());

        if da.can_build_certificate() {
            status_updates.send(Status::CreatingCert)?;
            break da.certify_dispersal().unwrap();
        }
    };

    if let Some(output) = output {
        std::fs::write(output, bincode::serialize(&cert)?)?;
    }

    if let Some(node) = node_addr {
        status_updates.send(Status::SendingCert)?;
        // TODO:

        if wait_for_inclusion {
            status_updates.send(Status::WaitingForInclusion)?;
        }
    }

    status_updates.send(Status::Done)?;
    Ok(())
}

pub enum Status {
    Encoding,
    Disseminating,
    WaitingAttestations,
    CreatingCert,
    SendingCert,
    WaitingForInclusion,
    Done,
}

impl Status {
    pub fn display(&self) -> &str {
        match self {
            Self::Encoding => "Encoding message into blob(s)",
            Self::Disseminating => "Sending blob(s) to the network",
            Self::WaitingAttestations => "Waiting for attestations",
            Self::CreatingCert => "Creating certificate(s)",
            Self::SendingCert => "Sending certificate(s) to node",
            Self::WaitingForInclusion => "Waiting for certificate(s) to be included in a block",
            Self::Done => "",
        }
    }
}

// To interact with the network service it's easier to just spawn
// an overwatch app
#[derive(Services)]
pub struct DisseminateApp {
    network: ServiceHandle<NetworkService<Libp2p>>,
    send_blob: ServiceHandle<DisseminateService>,
}

#[derive(Clone, Debug)]
pub struct Settings {
    // This is wrapped in an Arc just to make the struct Clone
    pub payload: Arc<Mutex<UnboundedReceiver<Box<[u8]>>>>,
    pub timeout: Duration,
    pub da_protocol: DaProtocolChoice,
    pub status_updates: Sender<Status>,
    pub node_addr: Option<Url>,
    pub output: Option<std::path::PathBuf>,
    pub wait_for_inclusion: bool,
}

pub struct DisseminateService {
    service_state: ServiceStateHandle<Self>,
}

impl ServiceData for DisseminateService {
    const SERVICE_ID: ServiceId = "Disseminate";
    type Settings = Settings;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = NoMessage;
}

#[async_trait::async_trait]
impl ServiceCore for DisseminateService {
    fn init(service_state: ServiceStateHandle<Self>) -> Result<Self, DynError> {
        Ok(Self { service_state })
    }

    async fn run(self) -> Result<(), DynError> {
        let Self { service_state } = self;
        let Settings {
            payload,
            timeout,
            da_protocol,
            status_updates,
            node_addr,
            output,
            wait_for_inclusion,
        } = service_state.settings_reader.get_updated_settings();

        match da_protocol {
            DaProtocolChoice {
                da_protocol: Protocol::FullReplication,
                settings:
                    ProtocolSettings {
                        full_replication: da_settings,
                    },
            } => {
                let network_relay = service_state
                    .overwatch_handle
                    .relay::<NetworkService<Libp2p>>()
                    .connect()
                    .await
                    .expect("Relay connection with NetworkService should succeed");

                while let Some(data) = payload.lock().await.recv().await {
                    match tokio::time::timeout(
                        timeout,
                        disseminate_and_wait(
                            FullReplication::new(AbsoluteNumber::new(da_settings.num_attestations)),
                            data,
                            Libp2pAdapter::new(network_relay.clone()).await,
                            status_updates.clone(),
                            node_addr.as_ref(),
                            output.as_ref(),
                            wait_for_inclusion,
                        ),
                    )
                    .await
                    {
                        Err(_) => {
                            tracing::error!(
                                "Timeout reached, check the logs for additional details"
                            );
                            std::process::exit(1);
                        }
                        Ok(Err(_)) => {
                            tracing::error!(
                                "Could not disseminate blob, check logs for additional details"
                            );
                            std::process::exit(1);
                        }
                        _ => {}
                    }
                }
            }
        }

        service_state.overwatch_handle.shutdown().await;
        Ok(())
    }
}

// This format is for clap args convenience, I could not
// find a way to use enums directly without having to implement
// parsing by hand.
// The `settings` field will hold the settings for all possible
// protocols, but only the one chosen will be used.
// We can enforce only sensible combinations of protocol/settings
// are specified by using special clap directives
#[derive(Clone, Debug, Args)]
pub struct DaProtocolChoice {
    #[clap(long, default_value = "full-replication")]
    pub da_protocol: Protocol,
    #[clap(flatten)]
    pub settings: ProtocolSettings,
}

#[derive(Clone, Debug, Args)]
pub struct ProtocolSettings {
    #[clap(flatten)]
    pub full_replication: FullReplicationSettings,
}

#[derive(Clone, Debug, ValueEnum)]
pub enum Protocol {
    FullReplication,
}

impl Default for FullReplicationSettings {
    fn default() -> Self {
        Self {
            num_attestations: 1,
        }
    }
}

#[derive(Debug, Clone, Args)]
pub struct FullReplicationSettings {
    #[clap(long, default_value = "1")]
    pub num_attestations: usize,
}
