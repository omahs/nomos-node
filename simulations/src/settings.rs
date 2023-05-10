use std::time::Duration;

use crate::network::NetworkSettings;
use crate::overlay::OverlaySettings;
use crate::streaming::StreamSettings;
use crate::warding::Ward;
use serde::Deserialize;
use serde_with::serde_as;

#[derive(Clone, Debug, Deserialize, Default)]
pub enum RunnerSettings {
    #[default]
    Sync,
    Async {
        chunks: usize,
    },
    Glauber {
        maximum_iterations: usize,
        update_rate: usize,
    },
    Layered {
        rounds_gap: usize,
        distribution: Option<Vec<f32>>,
    },
}

#[derive(Clone, Debug, Deserialize, Default)]
pub enum NodeSettings {
    Carnot,
    #[default]
    Dummy,
}

#[serde_with::serde_as]
#[derive(Default, Deserialize)]
pub struct SimulationSettings {
    #[serde(default)]
    pub wards: Vec<Ward>,
    pub network_settings: NetworkSettings,
    pub overlay_settings: OverlaySettings,
    pub node_settings: NodeSettings,
    pub runner_settings: RunnerSettings,
    pub stream_settings: StreamSettings,
    pub node_count: usize,
    pub views_count: usize,
    pub leaders_count: usize,
    pub seed: Option<u64>,
    #[serde_as(as = "serde_with::DurationSeconds")]
    pub sim_duration: Duration,
}
