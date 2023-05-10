// std
use rand::{seq::SliceRandom, Rng};
use std::collections::HashMap;
// crates
use serde::{Deserialize, Serialize};
// internal
use crate::{network::behaviour::NetworkBehaviour, node::NodeId};

use super::NetworkSettings;

#[derive(Debug, Copy, Clone, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Region {
    NorthAmerica,
    Europe,
    Asia,
    Africa,
    SouthAmerica,
    Australia,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionsData {
    pub regions: HashMap<Region, Vec<NodeId>>,
    #[serde(skip)]
    pub node_region: HashMap<NodeId, Region>,
    pub region_network_behaviour: HashMap<(Region, Region), NetworkBehaviour>,
}

impl RegionsData {
    pub fn new(
        regions: HashMap<Region, Vec<NodeId>>,
        region_network_behaviour: HashMap<(Region, Region), NetworkBehaviour>,
    ) -> Self {
        let node_region = regions
            .iter()
            .flat_map(|(region, nodes)| nodes.iter().copied().map(|node| (node, *region)))
            .collect();
        Self {
            regions,
            node_region,
            region_network_behaviour,
        }
    }

    pub fn node_region(&self, node_id: NodeId) -> Region {
        self.node_region[&node_id]
    }

    pub fn network_behaviour(&self, node_a: NodeId, node_b: NodeId) -> &NetworkBehaviour {
        let region_a = self.node_region[&node_a];
        let region_b = self.node_region[&node_b];
        self.region_network_behaviour
            .get(&(region_a, region_b))
            .or(self.region_network_behaviour.get(&(region_b, region_a)))
            .expect("Network behaviour not found for the given regions")
    }

    pub fn region_nodes(&self, region: Region) -> &[NodeId] {
        &self.regions[&region]
    }
}

// Takes a reference to the node_ids and simulation_settings and returns a HashMap
// representing the regions and their associated node IDs.
pub fn create_regions<R: Rng>(
    node_ids: &[NodeId],
    rng: &mut R,
    network_settings: &NetworkSettings,
) -> HashMap<Region, Vec<NodeId>> {
    let mut region_nodes = node_ids.to_vec();
    region_nodes.shuffle(rng);

    let regions = network_settings
        .regions
        .clone()
        .into_iter()
        .collect::<Vec<_>>();

    let last_region_index = regions.len() - 1;

    regions
        .iter()
        .enumerate()
        .map(|(i, (region, distribution))| {
            if i < last_region_index {
                let node_count = (node_ids.len() as f32 * distribution).round() as usize;
                let nodes = region_nodes.drain(..node_count).collect::<Vec<_>>();
                (*region, nodes)
            } else {
                // Assign the remaining nodes to the last region.
                (*region, region_nodes.clone())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use rand::rngs::mock::StepRng;

    use crate::{
        network::{
            regions::{create_regions, Region},
            NetworkDelays, NetworkSettings,
        },
        node::NodeId,
    };

    #[test]
    fn create_regions_precision() {
        struct TestCase {
            node_count: usize,
            distributions: Vec<f32>,
        }

        let test_cases = vec![
            TestCase {
                node_count: 10,
                distributions: vec![0.5, 0.3, 0.2],
            },
            TestCase {
                node_count: 7,
                distributions: vec![0.6, 0.4],
            },
            TestCase {
                node_count: 20,
                distributions: vec![0.4, 0.3, 0.2, 0.1],
            },
            TestCase {
                node_count: 23,
                distributions: vec![0.4, 0.3, 0.3],
            },
            TestCase {
                node_count: 111,
                distributions: vec![0.3, 0.3, 0.3, 0.1],
            },
            TestCase {
                node_count: 73,
                distributions: vec![0.3, 0.2, 0.2, 0.2, 0.1],
            },
        ];
        let mut rng = StepRng::new(1, 0);

        for tcase in test_cases.iter() {
            let nodes = (0..tcase.node_count)
                .map(Into::into)
                .collect::<Vec<NodeId>>();

            let available_regions = vec![
                Region::NorthAmerica,
                Region::Europe,
                Region::Asia,
                Region::Africa,
                Region::SouthAmerica,
                Region::Australia,
            ];

            let mut region_distribution = HashMap::new();
            for (region, &dist) in available_regions.iter().zip(&tcase.distributions) {
                region_distribution.insert(*region, dist);
            }

            let settings = NetworkSettings {
                network_behaviors: vec![NetworkDelays::default(); 6],
                regions: region_distribution,
            };

            let regions = create_regions(&nodes, &mut rng, &settings);

            let total_nodes_in_regions = regions.values().map(|v| v.len()).sum::<usize>();
            assert_eq!(total_nodes_in_regions, nodes.len());
        }
    }
}
