// std
use anyhow::Ok;
use serde::Serialize;
use simulations::streaming::io::IOSubscriber;
use simulations::streaming::naive::NaiveSubscriber;
use simulations::streaming::polars::PolarsSubscriber;
use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};
// crates
use clap::Parser;
use crossbeam::channel;
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::{Rng, SeedableRng};
use serde::de::DeserializeOwned;
use simulations::network::behaviour::create_behaviours;
use simulations::network::regions::{create_regions, RegionsData};
use simulations::network::{InMemoryNetworkInterface, Network};
use simulations::node::dummy::DummyNode;
use simulations::node::{Node, NodeId, OverlayState, ViewOverlay};
use simulations::overlay::{create_overlay, Overlay, SimulationOverlay};
use simulations::streaming::StreamType;
// internal
use simulations::{
    node::carnot::CarnotNode, output_processors::OutData, runner::SimulationRunner,
    settings::SimulationSettings,
};

/// Main simulation wrapper
/// Pipes together the cli arguments with the execution
#[derive(Parser)]
pub struct SimulationApp {
    /// Json file path, on `SimulationSettings` format
    #[clap(long, short)]
    input_settings: PathBuf,
    #[clap(long)]
    stream_type: StreamType,
}

impl SimulationApp {
    pub fn run(self) -> anyhow::Result<()> {
        let Self {
            input_settings,
            stream_type,
        } = self;
        let simulation_settings: SimulationSettings = load_json_from_file(&input_settings)?;

        let seed = simulation_settings.seed.unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("Time went backwards")
                .as_secs()
        });
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut node_ids: Vec<NodeId> = (0..simulation_settings.node_count)
            .map(Into::into)
            .collect();
        node_ids.shuffle(&mut rng);

        let regions = create_regions(&node_ids, &mut rng, &simulation_settings.network_settings);
        let behaviours = create_behaviours(&simulation_settings.network_settings);
        let regions_data = RegionsData::new(regions, behaviours);
        let overlay = create_overlay(&simulation_settings.overlay_settings);
        let overlays = generate_overlays(
            &node_ids,
            &overlay,
            simulation_settings.views_count,
            simulation_settings.leaders_count,
            &mut rng,
        );

        let overlay_state = Arc::new(RwLock::new(OverlayState {
            all_nodes: node_ids.clone(),
            overlay,
            overlays,
        }));

        let mut network = Network::new(regions_data);

        match &simulation_settings.node_settings {
            simulations::settings::NodeSettings::Carnot => {
                let nodes = node_ids
                    .iter()
                    .map(|node_id| CarnotNode::new(*node_id))
                    .collect();
                run(network, nodes, simulation_settings, stream_type)?;
            }
            simulations::settings::NodeSettings::Dummy => {
                let nodes = node_ids
                    .iter()
                    .map(|node_id| {
                        let (node_message_sender, node_message_receiver) = channel::unbounded();
                        let network_message_receiver =
                            network.connect(*node_id, node_message_receiver);
                        let network_interface = InMemoryNetworkInterface::new(
                            *node_id,
                            node_message_sender,
                            network_message_receiver,
                        );
                        DummyNode::new(*node_id, 0, overlay_state.clone(), network_interface)
                    })
                    .collect();
                run(network, nodes, simulation_settings, stream_type)?;
            }
        };
        Ok(())
    }
}

fn run<M, N: Node>(
    network: Network<M>,
    nodes: Vec<N>,
    settings: SimulationSettings,
    stream_type: StreamType,
) -> anyhow::Result<()>
where
    M: Clone + Send + Sync + 'static,
    N: Send + Sync + 'static,
    N::Settings: Clone + Send,
    N::State: Serialize,
{
    let stream_settings = settings.stream_settings.clone();
    let runner = SimulationRunner::<_, _, OutData>::new(network, nodes, settings);
    let p = Default::default();
    macro_rules! bail {
        ($producer: ident, $settings: ident, $sub: ident) => {
            let handle = runner.simulate($producer)?;
            let mut sub_handle = handle.subscribe::<$sub<OutData>>($settings)?;
            std::thread::spawn(move || {
                sub_handle.run();
            });
            handle.join()?;
        };
    }
    match stream_type {
        StreamType::Naive => {
            let settings = stream_settings.unwrap_naive();
            bail!(p, settings, NaiveSubscriber);
        }
        StreamType::IO => {
            let settings = stream_settings.unwrap_io();
            bail!(p, settings, IOSubscriber);
        }
        StreamType::Polars => {
            let settings = stream_settings.unwrap_polars();
            bail!(p, settings, PolarsSubscriber);
        }
    };

    Ok(handle.stop_after(sim_duration)?)
}

/// Generically load a json file
fn load_json_from_file<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let f = File::open(path).map_err(Box::new)?;
    Ok(serde_json::from_reader(f)?)
}

// Helper method to pregenerate views.
// TODO: Remove once shared overlay can generate new views on demand.
fn generate_overlays<R: Rng>(
    node_ids: &[NodeId],
    overlay: &SimulationOverlay,
    overlay_count: usize,
    leader_count: usize,
    rng: &mut R,
) -> BTreeMap<usize, ViewOverlay> {
    (0..overlay_count)
        .map(|view_id| {
            (
                view_id,
                ViewOverlay {
                    leaders: overlay.leaders(node_ids, leader_count, rng).collect(),
                    layout: overlay.layout(node_ids, rng),
                },
            )
        })
        .collect()
}

fn main() -> anyhow::Result<()> {
    let app: SimulationApp = SimulationApp::parse();
    app.run()?;
    Ok(())
}
