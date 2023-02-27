pub mod flat;

use crate::node::NodeId;
use rand::Rng;

pub trait Overlay {
    fn leaders<R: Rng>(
        &self,
        nodes: &[NodeId],
        size: usize,
        rng: &mut R,
    ) -> Box<dyn Iterator<Item = NodeId>>;
    fn layout<R: Rng>(
        &self,
        nodes: &[NodeId],
        rng: &mut R,
    ) -> Box<dyn Iterator<Item = Vec<NodeId>>>;
}