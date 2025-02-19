pub mod nodes;
use mixnet_node::MixnetNodeConfig;
use mixnet_topology::MixnetTopology;
pub use nodes::MixNode;
pub use nodes::NomosNode;
use once_cell::sync::Lazy;

// std
use std::net::TcpListener;
use std::time::Duration;
use std::{fmt::Debug, sync::Mutex};

//crates
use fraction::Fraction;
use rand::{thread_rng, Rng};

static NET_PORT: Lazy<Mutex<u16>> = Lazy::new(|| Mutex::new(thread_rng().gen_range(8000, 10000)));

pub fn get_available_port() -> u16 {
    let mut port = NET_PORT.lock().unwrap();
    *port += 1;
    while TcpListener::bind(("127.0.0.1", *port)).is_err() {
        *port += 1;
    }
    *port
}

#[async_trait::async_trait]
pub trait Node: Sized {
    type ConsensusInfo: Debug + Clone + PartialEq;
    async fn spawn_nodes(config: SpawnConfig) -> Vec<Self>;
    async fn consensus_info(&self) -> Self::ConsensusInfo;
    fn stop(&mut self);
}

#[derive(Clone)]
pub enum SpawnConfig {
    // Star topology: Every node is initially connected to a single node.
    Star {
        consensus: ConsensusConfig,
        mixnet: MixnetConfig,
    },
    // Chain topology: Every node is chained to the node next to it.
    Chain {
        consensus: ConsensusConfig,
        mixnet: MixnetConfig,
    },
}

#[derive(Clone)]
pub struct ConsensusConfig {
    pub n_participants: usize,
    pub threshold: Fraction,
    pub timeout: Duration,
}

#[derive(Clone)]
pub struct MixnetConfig {
    pub node_configs: Vec<MixnetNodeConfig>,
    pub topology: MixnetTopology,
}
