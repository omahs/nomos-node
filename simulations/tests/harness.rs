// std
use anyhow::Ok;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
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
use simulations::node::dummy::{DummyNode, Vote};
use simulations::node::{Node, NodeId, OverlayState, ViewOverlay};
use simulations::overlay::{create_overlay, Overlay, SimulationOverlay};
use simulations::streaming::StreamType;
// internal
use simulations::{
    node::carnot::CarnotNode, output_processors::OutData, runner::SimulationRunner,
    settings::SimulationSettings, streaming::io::IOProducer, streaming::naive::NaiveProducer,
    streaming::polars::PolarsProducer,
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
        let mut rng = SmallRng::seed_from_u64(simulation_settings.seed.unwrap_or(0));
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
            overlays: overlays.clone(),
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
                let nodes: HashMap<NodeId, DummyNode> = node_ids
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
                        (
                            *node_id,
                            DummyNode::new(*node_id, 0, overlay_state.clone(), network_interface),
                        )
                    })
                    .collect();

                // Next view leaders.
                let leaders = &overlays
                    .get(&1)
                    .ok_or_else(|| anyhow::Error::msg("no leaders"))?
                    .leaders;

                // Set initial messages from root nodes to next view leaders.
                overlays
                    .get(&0)
                    .ok_or_else(|| anyhow::Error::msg("no roots"))?
                    .layout
                    .committee_nodes(0.into())
                    .nodes
                    .iter()
                    .for_each(|r_id| {
                        leaders.iter().for_each(|l_id| {
                            nodes[r_id].send_message(*l_id, Vote::root_to_leader(1).into())
                        });
                    });

                run(
                    network,
                    Vec::from_iter(nodes.values().cloned()),
                    simulation_settings,
                    stream_type,
                )?;
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
    let sim_duration = settings.sim_duration;
    let runner = SimulationRunner::new(network, nodes, settings);

    let handle = match stream_type {
        simulations::streaming::StreamType::Naive => runner.simulate::<NaiveProducer<OutData>>()?,
        simulations::streaming::StreamType::Polars => {
            runner.simulate::<PolarsProducer<OutData>>()?
        }
        simulations::streaming::StreamType::IO => {
            runner.simulate::<IOProducer<std::io::Stdout, OutData>>()?
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
