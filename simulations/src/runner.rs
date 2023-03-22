use crate::node::carnot::{
    CarnotRole, CARNOT_INTERMEDIATE_STEPS, CARNOT_LEADER_STEPS, CARNOT_LEAF_STEPS,
    CARNOT_ROOT_STEPS, CARNOT_UNKNOWN_MESSAGE_RECEIVED_STEPS,
};
use crate::node::{Node, NodeId, StepTime};
use crate::overlay::Layout;
use rand::Rng;
use std::collections::HashMap;
use std::time::Duration;

pub struct ConsensusRunner<N: Node> {
    nodes: HashMap<NodeId, N>,
    leaders: Vec<NodeId>,
    layout: Layout,
}

#[allow(dead_code)]
#[derive(Debug, serde::Serialize)]
pub struct Report {
    round_time: Duration,
}

type Reducer = Box<dyn Fn(&[StepTime]) -> StepTime>;

impl<N: Node> ConsensusRunner<N>
where
    N::Settings: Clone,
{
    pub fn new<R: Rng>(
        mut rng: R,
        layout: Layout,
        leaders: Vec<NodeId>,
        node_settings: N::Settings,
    ) -> Self {
        let nodes = layout
            .node_ids()
            .map(|id| {
                let node = N::new(&mut rng, id, node_settings.clone());
                (id, node)
            })
            .collect();
        Self {
            nodes,
            layout,
            leaders,
        }
    }

    pub fn run(&mut self, reducer: Reducer) -> Report {
        let leaders = &self.leaders;
        let layout = &self.layout;

        let mut leader_times = leaders
            .iter()
            .map(|leader_node| {
                vec![self
                    .nodes
                    .get_mut(leader_node)
                    .unwrap()
                    .run_steps(CARNOT_LEADER_STEPS)]
            })
            .collect();

        let mut layer_times = Vec::new();
        for layer_nodes in layout
            .layers
            .values()
            .map(|committees| get_layer_nodes(committees, layout))
        {
            let times: Vec<StepTime> = layer_nodes
                .iter()
                .map(|(committee_id, node_id)| {
                    let steps = match layout.committees[committee_id].role {
                        CarnotRole::Root => CARNOT_ROOT_STEPS,
                        CarnotRole::Intermediate => CARNOT_INTERMEDIATE_STEPS,
                        CarnotRole::Leaf => CARNOT_LEAF_STEPS,
                        _ => {
                            // TODO: Should leader act as a leaf in a flat overlay?
                            CARNOT_UNKNOWN_MESSAGE_RECEIVED_STEPS
                        }
                    };
                    self.nodes.get_mut(node_id).unwrap().run_steps(steps)
                })
                .collect();

            layer_times.push(times)
        }

        layer_times.append(&mut leader_times);
        let round_time = layer_times.iter().map(|d| reducer(d)).sum();

        Report { round_time }
    }
}

fn get_layer_nodes(layer_committees: &[NodeId], layout: &Layout) -> Vec<(NodeId, NodeId)> {
    layer_committees
        .iter()
        .flat_map(|committee_id| get_committee_nodes(committee_id, layout))
        .collect()
}

fn get_committee_nodes(committee: &NodeId, layout: &Layout) -> Vec<(NodeId, NodeId)> {
    layout.committees[committee]
        .nodes
        .clone()
        .into_iter()
        .map(|node_id| (*committee, node_id))
        .collect()
}

#[cfg(test)]
mod test {
    use crate::{
        network::{
            behaviour::NetworkBehaviour,
            regions::{Region, RegionsData},
            Network,
        },
        node::{
            carnot::{CarnotNode, CarnotNodeSettings, CARNOT_STEPS_COSTS},
            NodeId, StepTime,
        },
        overlay::{
            flat::FlatOverlay,
            tree::{TreeOverlay, TreeSettings, TreeType},
            Overlay,
        },
        runner::{ConsensusRunner, Reducer},
    };
    use rand::{rngs::mock::StepRng, Rng};
    use std::{collections::HashMap, rc::Rc, time::Duration};

    fn setup_runner<R: Rng, O: Overlay<CarnotNode>>(
        mut rng: &mut R,
        overlay: &O,
    ) -> ConsensusRunner<CarnotNode> {
        let node_ids = overlay.nodes();
        let layout = overlay.layout(&node_ids, &mut rng);
        let leaders: Vec<NodeId> = overlay.leaders(&node_ids, 1, &mut rng).collect();

        let regions = std::iter::once((Region::Europe, node_ids.clone())).collect();
        let network_behaviour = std::iter::once((
            (Region::Europe, Region::Europe),
            NetworkBehaviour::new(Duration::from_millis(100), 0.0),
        ))
        .collect();

        let node_settings: CarnotNodeSettings = CarnotNodeSettings {
            steps_costs: CARNOT_STEPS_COSTS.iter().cloned().collect(),
            network: Network::new(RegionsData::new(regions, network_behaviour)),
            layout: overlay.layout(&node_ids, &mut rng),
            leaders: leaders.clone(),
        };

        ConsensusRunner::new(&mut rng, layout, leaders, Rc::new(node_settings))
    }

    #[test]
    fn run_flat_single_leader_steps() {
        let mut rng = StepRng::new(1, 0);
        let overlay = FlatOverlay::new(());

        let mut runner = setup_runner(&mut rng, &overlay);

        assert_eq!(
            Duration::from_millis(1100),
            runner
                .run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap()) as Reducer)
                .round_time
        );
    }

    #[test]
    fn run_tree_committee_1() {
        let mut rng = StepRng::new(1, 0);

        let overlay = TreeOverlay::new(TreeSettings {
            tree_type: TreeType::FullBinaryTree,
            depth: 3,
            committee_size: 1,
        });

        let mut runner: ConsensusRunner<CarnotNode> = setup_runner(&mut rng, &overlay);

        // # Leader (1 node):
        //
        // - 100ms - LeaderReceiveVote,
        // -   1s  - ValidateVote,
        //
        // Expected times [1.1s]

        // # Root (1 node):
        //
        // - 100ms - RootReceiveProposal,
        // -   1s  - ValidateProposal,
        // - 100ms - ReceiveVote,
        // -   1s  - ValidateVote,
        //
        // Expected times [2.2s]

        // # Intermediary (2 nodes):
        //
        // - 100ms - ReceiveProposal,
        // -   1s  - ValidateProposal,
        // - 100ms - ReceiveVote,
        // -   1s  - ValidateVote,
        //
        // Expected times [2.2s, 2.2s]

        // # Leaf (4 nodes):
        //
        // - 100ms - ReceiveProposal
        // -   1s  - ValidateProposal
        //
        // Expected times [1.1s, 1.1s, 1.1s, 1.1s]

        assert_eq!(
            Duration::from_millis(6600),
            runner
                .run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap()) as Reducer)
                .round_time
        );
    }

    #[test]
    fn run_tree_committee_100() {
        let mut rng = StepRng::new(1, 0);

        let overlay = TreeOverlay::new(TreeSettings {
            tree_type: TreeType::FullBinaryTree,
            depth: 3,
            committee_size: 100,
        });

        let mut runner: ConsensusRunner<CarnotNode> = setup_runner(&mut rng, &overlay);

        assert_eq!(
            Duration::from_millis(6600),
            runner
                .run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap()) as Reducer)
                .round_time
        );
    }

    #[test]
    fn run_tree_network_config_1() {
        let mut rng = StepRng::new(1, 0);

        let overlay = TreeOverlay::new(TreeSettings {
            tree_type: TreeType::FullBinaryTree,
            depth: 3,
            committee_size: 1,
        });

        let node_ids = overlay.nodes();
        let layout = overlay.layout(&node_ids, &mut rng);
        // Normaly a leaders would be selected randomly, here, we're assuming it's NodeID 1.
        // let leaders: Vec<NodeId> = overlay.leaders(&node_ids, 1, &mut rng).collect();
        let leaders = vec![1];

        let first_five = &node_ids[..5];
        let rest = &node_ids[5..];

        //       0
        //   1       2
        // 3   4   5   6
        //
        // # Leader - NodeID 1
        //
        // Sends vote to all committees.
        //
        // LeaderReceiveVote:
        // - 100ms - Asia - Asia (1 to 0, 1, 2, 3, 4)
        // - 500ms - Asia - Europe (1 to 5, 6)
        //
        // ValidateVote: 1s
        //
        // Expected times: [1.5s]

        // # Root - NodeID 0
        //
        // Sends vote to child committees.
        //
        // RootReceiveProposal:
        // - 100ms - Asia - Asia (0 to 1)
        //
        // ReceiveVote:
        // - 100ms - Asia - Asia (0 to 1, 2)
        //
        // No network:
        // - 1s - ValidateVote
        // - 1s - ValidateProposal
        //
        // Expected times: [2.2s]

        // # Intermediary - NodeID 1, 2:
        //
        // ReceiveVote:
        // - 100ms - Asia - Asia (1 to 3, 4)
        // - 500ms - Asia - Europe (2 to 5, 6)
        //
        // ReceiveProposal:
        // - 100ms - Asia - Asia (1 to 0, 2 to 0)
        //
        // No network:
        // - 1s - ValidateVote
        // - 1s - ValidateProposal
        //
        // Expected times [2.2s, 2.6s]

        // # Leaf - NodeID 3, 4, 5, 6
        //
        // ReceiveProposal:
        // - 100ms - Asia - Asia ( 3, 4 to 1)
        // - 500ms - Asia - Europe ( 5, 6 to 2)
        //
        // No network:
        // - 1s - ValidateProposal
        //
        // Expected times [1.1s, 1.1s, 1.5s, 1.5s]

        let regions = HashMap::from([
            (Region::Asia, first_five.to_vec()),
            (Region::Europe, rest.to_vec()),
        ]);

        let network_behaviour = HashMap::from([
            (
                (Region::Asia, Region::Asia),
                NetworkBehaviour::new(Duration::from_millis(100), 0.0),
            ),
            (
                (Region::Asia, Region::Europe),
                NetworkBehaviour::new(Duration::from_millis(500), 0.0),
            ),
            (
                (Region::Europe, Region::Europe),
                NetworkBehaviour::new(Duration::from_millis(100), 0.0),
            ),
        ]);

        let node_settings: CarnotNodeSettings = CarnotNodeSettings {
            steps_costs: CARNOT_STEPS_COSTS.iter().cloned().collect(),
            network: Network::new(RegionsData::new(regions, network_behaviour)),
            layout: overlay.layout(&node_ids, &mut rng),
            leaders: leaders.clone(),
        };

        let mut runner =
            ConsensusRunner::<CarnotNode>::new(&mut rng, layout, leaders, Rc::new(node_settings));

        assert_eq!(
            Duration::from_millis(7800),
            runner
                .run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap()) as Reducer)
                .round_time
        );
    }

    #[test]
    fn run_tree_network_config_100() {
        let mut rng = StepRng::new(1, 0);

        let overlay = TreeOverlay::new(TreeSettings {
            tree_type: TreeType::FullBinaryTree,
            depth: 3,
            committee_size: 100,
        });

        let node_ids = overlay.nodes();
        let layout = overlay.layout(&node_ids, &mut rng);
        // Normaly a leaders would be selected randomly, here, we're assuming it's NodeID 1.
        // let leaders: Vec<NodeId> = overlay.leaders(&node_ids, 1, &mut rng).collect();
        let leaders = vec![1];

        let two_thirds = node_ids.len() as f32 * 0.66;
        let rest = &node_ids[two_thirds as usize..];
        let two_thirds = &node_ids[..two_thirds as usize];

        let regions = HashMap::from([
            (Region::Asia, two_thirds.to_vec()),
            (Region::Europe, rest.to_vec()),
        ]);

        let network_behaviour = HashMap::from([
            (
                (Region::Asia, Region::Asia),
                NetworkBehaviour::new(Duration::from_millis(100), 0.0),
            ),
            (
                (Region::Asia, Region::Europe),
                NetworkBehaviour::new(Duration::from_millis(500), 0.0),
            ),
            (
                (Region::Europe, Region::Europe),
                NetworkBehaviour::new(Duration::from_millis(100), 0.0),
            ),
        ]);

        let node_settings: CarnotNodeSettings = CarnotNodeSettings {
            steps_costs: CARNOT_STEPS_COSTS.iter().cloned().collect(),
            network: Network::new(RegionsData::new(regions, network_behaviour)),
            layout: overlay.layout(&node_ids, &mut rng),
            leaders: leaders.clone(),
        };

        let mut runner =
            ConsensusRunner::<CarnotNode>::new(&mut rng, layout, leaders, Rc::new(node_settings));

        assert_eq!(
            Duration::from_millis(7800),
            runner
                .run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap()) as Reducer)
                .round_time
        );
    }

    #[test]
    fn run_tree_network_config_regions() {
        let mut rng = StepRng::new(1, 0);

        let overlay = TreeOverlay::new(TreeSettings {
            tree_type: TreeType::FullBinaryTree,
            depth: 4, // Increased depth to 4
            committee_size: 100,
        });

        let node_ids = overlay.nodes();
        let layout = overlay.layout(&node_ids, &mut rng);
        let leaders = vec![1];

        let region_size = node_ids.len() / 6;
        let regions = vec![
            Region::NorthAmerica,
            Region::Europe,
            Region::Asia,
            Region::Africa,
            Region::SouthAmerica,
            Region::Australia,
        ];

        let mut region_nodes = HashMap::new();
        for (index, region) in regions.iter().enumerate() {
            region_nodes.insert(
                *region,
                node_ids[index * region_size..(index + 1) * region_size].to_vec(),
            );
        }

        let network_behaviour = HashMap::from([
            (
                (Region::NorthAmerica, Region::Europe),
                NetworkBehaviour::new(Duration::from_millis(300), 0.0),
            ),
            (
                (Region::NorthAmerica, Region::Asia),
                NetworkBehaviour::new(Duration::from_millis(400), 0.0),
            ),
        ]);

        // let node_settings: CarnotNodeSettings = CarnotNodeSettings {
        //     steps_costs: CARNOT_STEPS_COSTS.iter().cloned().collect(),
        //     network: Network::new(RegionsData::new(region_nodes, network_behaviour)),
        //     layout: overlay.layout(&node_ids, &mut rng),
        //     leaders: leaders.clone(),
        // };

        // let mut runner =
        //     ConsensusRunner::<CarnotNode>::new(&mut rng, layout, leaders, Rc::new(node_settings));

        // assert_eq!(
        //     Duration::from_millis(11000),
        //     runner
        //         .run(Box::new(|times: &[StepTime]| *times.iter().max().unwrap()) as Reducer)
        //         .round_time
        // );
    }
}
