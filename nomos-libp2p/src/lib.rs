use std::error::Error;
use std::pin::Pin;
use std::task::{Context, Poll};

use std::time::Duration;

pub use libp2p;

use blake2::digest::{consts::U32, Digest};
use blake2::Blake2b;
use libp2p::gossipsub::{Message, MessageId, TopicHash};

pub use libp2p::{
    core::upgrade,
    dns,
    gossipsub::{self, PublishError, SubscriptionError},
    identity::{self, secp256k1},
    plaintext::PlainText2Config,
    swarm::{
        dial_opts::DialOpts, DialError, NetworkBehaviour, SwarmBuilder, SwarmEvent, THandlerErr,
    },
    tcp, yamux, PeerId, Transport,
};
use libp2p::{swarm::ConnectionId, tcp::tokio::Tcp};
pub use multiaddr::{multiaddr, Multiaddr, Protocol};
use serde::{Deserialize, Serialize};

/// Wraps [`libp2p::Swarm`], and config it for use within Nomos.
pub struct Swarm {
    // A core libp2p swarm
    swarm: libp2p::Swarm<Behaviour>,
}

#[derive(NetworkBehaviour)]
pub struct Behaviour {
    gossipsub: gossipsub::Behaviour,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwarmConfig {
    // Listening IPv4 address
    pub host: std::net::Ipv4Addr,
    // TCP listening port. Use 0 for random
    pub port: u16,
    // Secp256k1 private key in Hex format (`0x123...abc`). Default random
    #[serde(with = "secret_key_serde", default = "secp256k1::SecretKey::generate")]
    pub node_key: secp256k1::SecretKey,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            host: std::net::Ipv4Addr::new(0, 0, 0, 0),
            port: 60000,
            node_key: secp256k1::SecretKey::generate(),
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum SwarmError {
    #[error("duplicate dialing")]
    DuplicateDialing,
}

/// A timeout for the setup and protocol upgrade process for all in/outbound connections
const TRANSPORT_TIMEOUT: Duration = Duration::from_secs(20);

impl Swarm {
    /// Builds a [`Swarm`] configured for use with Nomos on top of a tokio executor.
    //
    // TODO: define error types
    pub fn build(config: &SwarmConfig) -> Result<Self, Box<dyn Error>> {
        let id_keys = identity::Keypair::from(secp256k1::Keypair::from(config.node_key.clone()));
        let local_peer_id = PeerId::from(id_keys.public());
        log::info!("libp2p peer_id:{}", local_peer_id);

        // TODO: consider using noise authentication
        let tcp_transport = tcp::Transport::<Tcp>::new(tcp::Config::default().nodelay(true))
            .upgrade(upgrade::Version::V1Lazy)
            .authenticate(PlainText2Config {
                local_public_key: id_keys.public(),
            })
            .multiplex(yamux::Config::default())
            .timeout(TRANSPORT_TIMEOUT)
            .boxed();

        // Wrapping TCP transport into DNS transport to resolve hostnames.
        let tcp_transport = dns::TokioDnsConfig::system(tcp_transport)?.boxed();

        // TODO: consider using Signed or Anonymous.
        //       For Anonymous, a custom `message_id` function need to be set
        //       to prevent all messages from a peer being filtered as duplicates.
        let gossipsub = gossipsub::Behaviour::new(
            gossipsub::MessageAuthenticity::Author(local_peer_id),
            gossipsub::ConfigBuilder::default()
                .validation_mode(gossipsub::ValidationMode::None)
                .message_id_fn(compute_message_id)
                .heartbeat_initial_delay(Duration::from_secs(1))
                .build()?,
        )?;

        let mut swarm = SwarmBuilder::with_tokio_executor(
            tcp_transport,
            Behaviour { gossipsub },
            local_peer_id,
        )
        .build();

        swarm.listen_on(multiaddr!(Ip4(config.host), Tcp(config.port)))?;

        Ok(Swarm { swarm })
    }

    /// Initiates a connection attempt to a peer
    pub fn connect(&mut self, peer_addr: Multiaddr) -> Result<ConnectionId, DialError> {
        let opt = DialOpts::from(peer_addr.clone());
        let connection_id = opt.connection_id();

        tracing::debug!("attempting to dial {peer_addr}. connection_id:{connection_id:?}",);
        self.swarm.dial(opt)?;
        Ok(connection_id)
    }

    /// Subscribes to a topic
    ///
    /// Returns true if the topic is newly subscribed or false if already subscribed.
    pub fn subscribe(&mut self, topic: &str) -> Result<bool, SubscriptionError> {
        self.swarm
            .behaviour_mut()
            .gossipsub
            .subscribe(&gossipsub::IdentTopic::new(topic))
    }

    pub fn broadcast(
        &mut self,
        topic: &str,
        message: impl Into<Vec<u8>>,
    ) -> Result<MessageId, PublishError> {
        self.swarm
            .behaviour_mut()
            .gossipsub
            .publish(gossipsub::IdentTopic::new(topic), message)
    }

    /// Unsubscribes from a topic
    ///
    /// Returns true if previously subscribed
    pub fn unsubscribe(&mut self, topic: &str) -> Result<bool, PublishError> {
        self.swarm
            .behaviour_mut()
            .gossipsub
            .unsubscribe(&gossipsub::IdentTopic::new(topic))
    }

    /// Returns a reference to the underlying [`libp2p::Swarm`]
    pub fn swarm(&self) -> &libp2p::Swarm<Behaviour> {
        &self.swarm
    }

    pub fn is_subscribed(&mut self, topic: &str) -> bool {
        let topic_hash = Self::topic_hash(topic);

        //TODO: consider O(1) searching by having our own data structure
        self.swarm
            .behaviour_mut()
            .gossipsub
            .topics()
            .any(|h| h == &topic_hash)
    }

    pub fn topic_hash(topic: &str) -> TopicHash {
        gossipsub::IdentTopic::new(topic).hash()
    }
}

impl futures::Stream for Swarm {
    type Item = SwarmEvent<BehaviourEvent, THandlerErr<Behaviour>>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.swarm).poll_next(cx)
    }
}

fn compute_message_id(message: &Message) -> MessageId {
    let mut hasher = Blake2b::<U32>::new();
    hasher.update(&message.data);
    MessageId::from(hasher.finalize().to_vec())
}

mod secret_key_serde {
    use libp2p::identity::secp256k1;
    use serde::de::Error;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(key: &secp256k1::SecretKey, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_str = hex::encode(key.to_bytes());
        hex_str.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<secp256k1::SecretKey, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_str = String::deserialize(deserializer)?;
        let mut key_bytes = hex::decode(hex_str).map_err(|e| D::Error::custom(format!("{e}")))?;
        secp256k1::SecretKey::try_from_bytes(key_bytes.as_mut_slice())
            .map_err(|e| D::Error::custom(format!("{e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_serde() {
        let config: SwarmConfig = Default::default();

        let serialized = serde_json::to_string(&config).unwrap();
        println!("{serialized}");

        let deserialized: SwarmConfig = serde_json::from_str(serialized.as_str()).unwrap();
        assert_eq!(deserialized.host, config.host);
        assert_eq!(deserialized.port, config.port);
        assert_eq!(deserialized.node_key.to_bytes(), config.node_key.to_bytes());
    }
}
