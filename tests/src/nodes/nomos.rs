// std
use std::net::SocketAddr;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
// internal
use crate::{get_available_port, Node, SpawnConfig};
use consensus_engine::overlay::{FlatOverlaySettings, RoundRobin};
use consensus_engine::NodeId;
use mixnet_client::{MixnetClientConfig, MixnetClientMode};
use mixnet_node::MixnetNodeConfig;
use mixnet_topology::MixnetTopology;
use nomos_consensus::{CarnotInfo, CarnotSettings};
use nomos_http::backends::axum::AxumBackendSettings;
use nomos_libp2p::Multiaddr;
use nomos_log::{LoggerBackend, LoggerFormat};
use nomos_mempool::{backend::Status, MempoolMetrics};
use nomos_network::backends::libp2p::{Libp2pConfig, Libp2pInfo};
use nomos_network::NetworkConfig;
use nomos_node::Config;
// crates
use fraction::Fraction;
use once_cell::sync::Lazy;
use rand::{thread_rng, Rng};
use reqwest::Client;
use serde::Serialize;
use tempfile::NamedTempFile;

static CLIENT: Lazy<Client> = Lazy::new(Client::new);
const NOMOS_BIN: &str = "../target/debug/nomos-node";
const CARNOT_INFO_API: &str = "carnot/info";
const NETWORK_INFO_API: &str = "network/info";
const MEMPOOL_API: &str = "mempool-";
const LOGS_PREFIX: &str = "__logs";

pub struct NomosNode {
    addr: SocketAddr,
    _tempdir: tempfile::TempDir,
    child: Child,
    config: Config,
}

impl Drop for NomosNode {
    fn drop(&mut self) {
        if std::thread::panicking() {
            println!("persisting directory at {}", self._tempdir.path().display());
            // we need ownership of the dir to persist it
            let dir = std::mem::replace(&mut self._tempdir, tempfile::tempdir().unwrap());
            // a bit confusing but `into_path` persists the directory
            let _ = dir.into_path();
        }

        self.child.kill().unwrap();
    }
}

impl NomosNode {
    pub async fn spawn(mut config: Config) -> Self {
        // Waku stores the messages in a db file in the current dir, we need a different
        // directory for each node to avoid conflicts
        let dir = tempfile::tempdir().unwrap();
        let mut file = NamedTempFile::new().unwrap();
        let config_path = file.path().to_owned();

        // setup logging so that we can intercept it later in testing
        config.log.backend = LoggerBackend::File {
            directory: dir.path().to_owned(),
            prefix: Some(LOGS_PREFIX.into()),
        };
        config.log.format = LoggerFormat::Json;

        serde_yaml::to_writer(&mut file, &config).unwrap();
        let child = Command::new(std::env::current_dir().unwrap().join(NOMOS_BIN))
            .arg(&config_path)
            .current_dir(dir.path())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let node = Self {
            addr: config.http.backend.address,
            child,
            _tempdir: dir,
            config,
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            node.wait_online().await
        })
        .await
        .unwrap();

        node
    }

    async fn get(&self, path: &str) -> reqwest::Result<reqwest::Response> {
        CLIENT
            .get(format!("http://{}/{}", self.addr, path))
            .send()
            .await
    }

    async fn post(&self, path: &str, body: Vec<u8>) -> reqwest::Result<reqwest::Response> {
        CLIENT
            .post(format!("http://{}/{}", self.addr, path))
            .body(body)
            .send()
            .await
    }

    async fn wait_online(&self) {
        while self.get(CARNOT_INFO_API).await.is_err() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    pub async fn get_listening_address(&self) -> Multiaddr {
        self.get(NETWORK_INFO_API)
            .await
            .unwrap()
            .json::<Libp2pInfo>()
            .await
            .unwrap()
            .listen_addresses
            .swap_remove(0)
    }

    pub async fn mempoool_metrics(&self, pool: Pool) -> MempoolMetrics {
        let discr = match pool {
            Pool::Cl => "cl",
            Pool::Da => "da",
        };
        let addr = format!("{}{}/metrics", MEMPOOL_API, discr);
        let res = self
            .get(&addr)
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        MempoolMetrics {
            pending_items: res["pending_items"].as_u64().unwrap() as usize,
            last_item_timestamp: res["last_item"].as_u64().unwrap(),
        }
    }

    pub async fn mempoool_status<K>(&self, pool: Pool, ids: Vec<K>) -> Vec<Status>
    where
        K: Serialize,
    {
        let discr = match pool {
            Pool::Cl => "cl",
            Pool::Da => "da",
        };
        let addr = format!("{}{}/status", MEMPOOL_API, discr);
        let res = self
            .post(&addr, serde_json::to_string(&ids).unwrap().into())
            .await
            .unwrap();
        println!("res: {:?}", res);
        self.post(&addr, serde_json::to_string(&ids).unwrap().into())
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    pub async fn send_mempool_item(&self, pool: Pool, item: Vec<u8>) {
        let discr = match pool {
            Pool::Cl => "cl",
            Pool::Da => "da",
        };
        let addr = format!("{}{}/add", MEMPOOL_API, discr);
        self.post(&addr, item).await.unwrap();
    }

    // not async so that we can use this in `Drop`
    pub fn get_logs_from_file(&self) -> String {
        println!(
            "fetching logs from dir {}...",
            self._tempdir.path().display()
        );
        // std::thread::sleep(std::time::Duration::from_secs(50));
        std::fs::read_dir(self._tempdir.path())
            .unwrap()
            .filter_map(|entry| {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_file() && path.to_str().unwrap().contains(LOGS_PREFIX) {
                    Some(path)
                } else {
                    None
                }
            })
            .map(|f| std::fs::read_to_string(f).unwrap())
            .collect::<String>()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }
}

#[async_trait::async_trait]
impl Node for NomosNode {
    type ConsensusInfo = CarnotInfo;

    async fn spawn_nodes(config: SpawnConfig) -> Vec<Self> {
        match config {
            SpawnConfig::Star {
                n_participants,
                threshold,
                timeout,
                mut mixnet_node_configs,
                mixnet_topology,
            } => {
                let mut ids = vec![[0; 32]; n_participants];
                for id in &mut ids {
                    thread_rng().fill(id);
                }
                let mut configs = ids
                    .iter()
                    .map(|id| {
                        create_node_config(
                            ids.iter().copied().map(NodeId::new).collect(),
                            *id,
                            threshold,
                            timeout,
                            mixnet_node_configs.pop(),
                            mixnet_topology.clone(),
                        )
                    })
                    .collect::<Vec<_>>();
                let mut nodes = vec![Self::spawn(configs.swap_remove(0)).await];
                let listening_addr = nodes[0].get_listening_address().await;
                for mut conf in configs {
                    conf.network
                        .backend
                        .initial_peers
                        .push(listening_addr.clone());

                    nodes.push(Self::spawn(conf).await);
                }
                nodes
            }
        }
    }

    async fn consensus_info(&self) -> Self::ConsensusInfo {
        self.get(CARNOT_INFO_API)
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    fn stop(&mut self) {
        self.child.kill().unwrap();
    }
}

fn create_node_config(
    nodes: Vec<NodeId>,
    private_key: [u8; 32],
    threshold: Fraction,
    timeout: Duration,
    mixnet_node_config: Option<MixnetNodeConfig>,
    mixnet_topology: MixnetTopology,
) -> Config {
    let mixnet_client_mode = match mixnet_node_config {
        Some(node_config) => MixnetClientMode::SenderReceiver(node_config.client_listen_address),
        None => MixnetClientMode::Sender,
    };

    let mut config = Config {
        network: NetworkConfig {
            backend: Libp2pConfig {
                inner: Default::default(),
                initial_peers: vec![],
                mixnet_client: MixnetClientConfig {
                    mode: mixnet_client_mode,
                    topology: mixnet_topology,
                    connection_pool_size: 255,
                    max_retries: 3,
                    retry_delay: Duration::from_secs(5),
                },
                mixnet_delay: Duration::ZERO..Duration::from_millis(10),
            },
        },
        consensus: CarnotSettings {
            private_key,
            overlay_settings: FlatOverlaySettings {
                nodes,
                leader: RoundRobin::new(),
                // By setting the leader_threshold to 1 we ensure that all nodes come
                // online before progressing. This is only necessary until we add a way
                // to recover poast blocks from other nodes.
                leader_super_majority_threshold: Some(threshold),
            },
            timeout,
            transaction_selector_settings: (),
            blob_selector_settings: (),
        },
        log: Default::default(),
        http: nomos_http::http::HttpServiceSettings {
            backend: AxumBackendSettings {
                address: format!("127.0.0.1:{}", get_available_port())
                    .parse()
                    .unwrap(),
                cors_origins: vec![],
            },
        },
        #[cfg(feature = "metrics")]
        metrics: Default::default(),
        da: nomos_da::Settings {
            da_protocol: full_replication::Settings {
                num_attestations: 1,
            },
            backend: nomos_da::backend::memory_cache::BlobCacheSettings {
                max_capacity: usize::MAX,
                evicting_period: Duration::from_secs(60 * 60 * 24), // 1 day
            },
        },
    };

    config.network.backend.inner.port = get_available_port();

    config
}

pub enum Pool {
    Da,
    Cl,
}
