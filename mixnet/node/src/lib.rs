mod client_notifier;
pub mod config;

use std::{net::SocketAddr, time::Duration};

use client_notifier::ClientNotifier;
pub use config::MixnetNodeConfig;
use mixnet_protocol::{Body, ProtocolError};
use mixnet_topology::MixnetNodeId;
use mixnet_util::{ConnectionPool, MessagePool};
use nym_sphinx::{
    addressing::nodes::{NymNodeRoutingAddress, NymNodeRoutingAddressError},
    Delay, DestinationAddressBytes, NodeAddressBytes, Payload, PrivateKey,
};
pub use sphinx_packet::crypto::PRIVATE_KEY_SIZE;
use sphinx_packet::{crypto::PUBLIC_KEY_SIZE, ProcessedPacket, SphinxPacket};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc::{self, Receiver},
};

pub type Result<T> = core::result::Result<T, MixnetNodeError>;

#[derive(Debug, thiserror::Error)]
pub enum MixnetNodeError {
    #[error("{0}")]
    Protocol(#[from] ProtocolError),
    #[error("invalid routing address: {0}")]
    InvalidRoutingAddress(#[from] NymNodeRoutingAddressError),
    #[error("send error: {0}")]
    SendError(#[from] tokio::sync::mpsc::error::TrySendError<Body>),
    #[error("client: {0}")]
    Client(ProtocolError),
}

// A mix node that routes packets in the Mixnet.
pub struct MixnetNode {
    config: MixnetNodeConfig,
    message_pool: MessagePool<Message>,
}

impl MixnetNode {
    pub fn new(config: MixnetNodeConfig) -> Self {
        Self {
            config,
            message_pool: MessagePool::new(),
        }
    }

    pub fn id(&self) -> MixnetNodeId {
        self.public_key()
    }

    pub fn public_key(&self) -> [u8; PUBLIC_KEY_SIZE] {
        self.config.public_key()
    }

    const CLIENT_NOTI_CHANNEL_SIZE: usize = 100;

    pub async fn run(self) -> Result<()> {
        tracing::info!("Public key: {:?}", self.public_key());

        // Spawn a ClientNotifier
        let (client_tx, client_rx) = mpsc::channel(Self::CLIENT_NOTI_CHANNEL_SIZE);
        tokio::spawn(async move {
            if let Err(e) = ClientNotifier::run(self.config.client_listen_address, client_rx).await
            {
                tracing::error!("failed to run client notifier: {e}");
            }
        });

        //TODO: Accepting ad-hoc TCP conns for now. Improve conn handling.
        //TODO: Add graceful shutdown
        let listener = TcpListener::bind(self.config.listen_address)
            .await
            .map_err(ProtocolError::IO)?;
        tracing::info!(
            "Listening mixnet node connections: {}",
            self.config.listen_address
        );

        MixnetNodeRunner {
            config: self.config,
            message_pool: self.message_pool,
            client_tx,
        };
        loop {
            match listener.accept().await {
                Ok((socket, remote_addr)) => {
                    tracing::debug!("Accepted incoming connection from {remote_addr:?}");

                    let runner = self.runner();
                    tokio::spawn(async move {
                        if let Err(e) = runner.handle_connection(socket).await {
                            tracing::error!("failed to handle conn: {e}");
                        }
                    });
                }
                Err(e) => tracing::warn!("Failed to accept incoming connection: {e}"),
            }
        }
        Ok(())
    }

    fn runner(&self) -> MixnetNodeRunner {
        MixnetNodeRunner {
            config: self.config,
            message_pool: self.message_pool.clone(),
            client_tx: self.client_tx.clone(),
        }
    }
}

struct Message {
    // number of retries already done
    retries: usize,
    body: Body,
}

struct MixnetNodeRunner {
    config: MixnetNodeConfig,
    message_pool: MessagePool<Message>,
    client_tx: mpsc::Sender<Body>,
}

impl MixnetNodeRunner {
    async fn handle_connection(&self, mut socket: TcpStream) -> Result<()> {
        loop {
            let body = Body::read(&mut socket).await?;

            tokio::spawn(async move {
                if let Err(e) = self.handle_body(Message { retries: 0, body }).await {
                    tracing::error!("failed to handle body: {e}");
                }
            });
        }
    }

    async fn handle_body(&self, msg: Message) -> Result<()> {
        match msg.body {
            Body::SphinxPacket(packet) => {
                self.handle_sphinx_packet(Message {
                    retries: msg.retries,
                    body: Body::SphinxPacket(packet),
                })
                .await
            }
            Body::FinalPayload(payload) => {
                self.forward_body_to_client_notifier(Body::FinalPayload(payload))
                    .await
            }
            _ => unreachable!(),
        }
    }

    async fn handle_sphinx_packet(&self, packet: Message) -> Result<()> {
        match packet
            .process(&PrivateKey::from(self.config.private_key))
            .map_err(ProtocolError::InvalidSphinxPacket)?
        {
            ProcessedPacket::ForwardHop(packet, next_node_addr, delay) => {
                self.forward_packet_to_next_hop(packet, next_node_addr, delay)
                    .await
            }
            ProcessedPacket::FinalHop(destination_addr, _, payload) => {
                self.forward_payload_to_destination(payload, destination_addr)
                    .await
            }
        }
    }

    async fn forward_body_to_client_notifier(&self, body: Body) -> Result<()> {
        // TODO: Decrypt the final payload using the private key, if it's encrypted

        // Do not wait when the channel is full or no receiver exists
        self.client_tx.try_send(body)?;
        Ok(())
    }

    async fn forward_packet_to_next_hop(
        &self,
        packet: Message,
        next_node_addr: NodeAddressBytes,
        delay: Delay,
    ) -> Result<()> {
        tracing::debug!("Delaying the packet for {delay:?}");
        tokio::time::sleep(delay.to_duration()).await;

        self.forward(packet, NymNodeRoutingAddress::try_from(next_node_addr)?)
            .await
    }

    async fn forward_payload_to_destination(
        &self,
        payload: Message,
        destination_addr: DestinationAddressBytes,
    ) -> Result<()> {
        tracing::debug!("Forwarding final payload to destination mixnode");

        self.forward(
            payload,
            NymNodeRoutingAddress::try_from_bytes(&destination_addr.as_bytes())?,
        )
        .await
    }

    async fn forward(&self, msg: Message, to: NymNodeRoutingAddress) -> Result<()> {
        let addr = SocketAddr::from(to);

        let body = if let Some(handle) = self.message_pool.get(&addr) {
            if let Err(err) = handle.send(body).await {
                err.0
            } else {
                // If succesfull send message to msg writting thread,
                // then we just return
                return Ok(());
            }
        } else {
            body
        };

        // the target msg handling thread does not live,
        // so we need to create a new one
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(body).unwrap();

        let this = MessageHandler {
            config: self.config,
            remote_addr: addr,
            receiver: rx,
            // TODO: do we need to retry on error for the tcp stream here?
            conn: TcpStream::connect(addr).await?,
        };

        let handle = tokio::spawn(this.run());
        self.message_pool.insert(addr, handle);
        Ok(())
    }
}

struct MessageHandler {
    config: MixnetNodeConfig,
    remote_addr: SocketAddr,
    receiver: Receiver<Message>,
    conn: TcpStream,
}

impl MessageHandler {
    pub fn new(
        remote_addr: SocketAddr,
        conn: TcpStream,
        receiver: Receiver<Message>,
        config: MixnetNodeConfig,
    ) -> Self {
        Self {
            receiver,
            remote_addr,
            conn,
            config,
        }
    }

    pub async fn run(mut self) {
        loop {
            tokio::select! {
                msg = self.receiver.recv() => {
                    if let Some(msg) = msg {
                        // TODO: handle msg sending
                    } else {
                        break;
                    }
                },
                _ = tokio::signal::ctrl_c() => {
                    tracing::info!("Shutting down message handler thread for {}...", self.remote_addr);
                    break;
                }
            }
        }
    }
}
