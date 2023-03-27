use crate::node::Node;
use crate::warding::{SimulationState, SimulationWard};
use serde::Deserialize;

/// Time to finality ward. It monitors the amount of rounds of the simulations, triggers when surpassing
/// the set threshold.
#[derive(Debug, Deserialize, Copy, Clone)]
pub struct MaxViewWard {
    max_view: usize,
}

impl<N: Node> SimulationWard<N> for MaxViewWard {
    type SimulationState = SimulationState<N>;
    fn analyze(&mut self, state: &Self::SimulationState) -> bool {
        let x = state.nodes.read().unwrap();
        for node in x.iter() {
            if node.current_view() >= self.max_view {
                return false;
            }
        }
        true
    }
}

#[cfg(test)]
mod test {
    use crate::node::{Node, NodeId};
    use crate::warding::ttf::MaxViewWard;
    use crate::warding::{SimulationState, SimulationWard};
    use rand::Rng;
    use std::ops::AddAssign;
    use std::sync::{Arc, RwLock};

    #[test]
    fn rebase_threshold() {
        impl Node for usize {
            type Settings = ();
            type State = Self;

            fn new<R: Rng>(rng: &mut R, id: NodeId, settings: Self::Settings) -> Self {
                id.inner()
            }

            fn id(&self) -> NodeId {
                (*self).into()
            }

            fn current_view(&self) -> usize {
                *self
            }

            fn state(&self) -> &Self::State {
                self
            }

            fn step(&mut self) {
                self.add_assign(1);
            }
        }
        let mut ttf = MaxViewWard { max_view: 10 };

        let node = 9;
        let state = SimulationState {
            nodes: Arc::new(RwLock::new(vec![node])),
        };
        assert!(ttf.analyze(&state));

        state.nodes.write().unwrap().push(11);
        assert!(!ttf.analyze(&state));
    }
}
