mod async_runner;
mod glauber_runner;
mod layered_runner;
mod sync_runner;

// std
use std::sync::Arc;
use std::time::Duration;

use crate::output_processors::Record;
use crate::streaming::polars::ToSeries;
// crates
use crate::streaming::{
    runtime_subscriber::RuntimeSubscriber, settings_subscriber::SettingsSubscriber, StreamProducer,
    Subscriber, SubscriberHandle,
};
use crossbeam::channel::Sender;
use parking_lot::RwLock;
use rand::rngs::SmallRng;
use rand::{RngCore, SeedableRng};
use rayon::prelude::*;
use serde::Serialize;

// internal
use crate::network::Network;
use crate::node::Node;
use crate::settings::{RunnerSettings, SimulationSettings};
use crate::warding::{SimulationState, SimulationWard, Ward};

pub struct SimulationRunnerHandle<R> {
    producer: StreamProducer<R>,
    stop_tx: Sender<()>,
    handle: std::thread::JoinHandle<anyhow::Result<()>>,
}

impl<R: Record> SimulationRunnerHandle<R> {
    pub fn stop_after(self, duration: Duration) -> anyhow::Result<()> {
        std::thread::sleep(duration);
        self.stop()
    }

    pub fn stop(self) -> anyhow::Result<()> {
        if !self.handle.is_finished() {
            self.stop_tx.send(())?;
            self.producer.stop()?;
        }
        Ok(())
    }

    pub fn subscribe<S: Subscriber<Record = R>>(
        &self,
        settings: S::Settings,
    ) -> anyhow::Result<SubscriberHandle<S>> {
        self.producer.subscribe(settings)
    }

    pub fn join(self) -> anyhow::Result<()> {
        self.handle.join().expect("Join simulation thread")
    }
}

pub(crate) struct SimulationRunnerInner<M> {
    network: Network<M>,
    wards: Vec<Ward>,
    rng: SmallRng,
}

impl<M> SimulationRunnerInner<M>
where
    M: Send + Sync + Clone,
{
    fn check_wards<N>(&mut self, state: &SimulationState<N>) -> bool
    where
        N: Node + Send + Sync,
        N::Settings: Clone + Send,
        N::State: Serialize,
    {
        self.wards
            .par_iter_mut()
            .map(|ward| ward.analyze(state))
            .any(|x| x)
    }

    fn step<N>(&mut self, nodes: &mut [N])
    where
        N: Node + Send + Sync,
        N::Settings: Clone + Send,
        N::State: Serialize,
    {
        self.network.dispatch_after(Duration::from_millis(100));
        nodes.par_iter_mut().for_each(|node| {
            node.step();
        });
        self.network.collect_messages();
    }
}

/// Encapsulation solution for the simulations runner
/// Holds the network state, the simulating nodes and the simulation settings.
pub struct SimulationRunner<M, N, R>
where
    N: Node,
{
    inner: SimulationRunnerInner<M>,
    nodes: Arc<RwLock<Vec<N>>>,
    runner_settings: RunnerSettings,
    producer: StreamProducer<R>,
}

impl<M, N: Node, R> SimulationRunner<M, N, R>
where
    M: Clone + Send + Sync + 'static,
    N: Send + Sync + 'static,
    N::Settings: Clone + Send,
    N::State: Serialize,
    R: Record
        + for<'a> TryFrom<&'a SimulationState<N>, Error = anyhow::Error>
        + Send
        + Sync
        + 'static,
{
    pub fn new(
        network: Network<M>,
        nodes: Vec<N>,
        producer: StreamProducer<R>,
        mut settings: SimulationSettings,
    ) -> anyhow::Result<Self> {
        let seed = settings
            .seed
            .unwrap_or_else(|| rand::thread_rng().next_u64());

        settings
            .seed
            .get_or_insert_with(|| rand::thread_rng().next_u64());

        // Store the settings to the producer so that we can collect them later
        producer.send(R::from(settings.clone()))?;

        let rng = SmallRng::seed_from_u64(seed);
        let nodes = Arc::new(RwLock::new(nodes));
        let SimulationSettings {
            wards,
            overlay_settings: _,
            node_settings: _,
            runner_settings,
            stream_settings: _,
            node_count: _,
            seed: _,
            views_count: _,
            leaders_count: _,
            network_settings: _,
            step_time: _,
        } = settings;
        Ok(Self {
            runner_settings,
            inner: SimulationRunnerInner {
                network,
                rng,
                wards,
            },
            nodes,
            producer,
        })
    }

    pub fn simulate(self) -> anyhow::Result<SimulationRunnerHandle<R>> {
        // init the start time
        let _ = *crate::START_TIME;

        match self.runner_settings.clone() {
            RunnerSettings::Sync => sync_runner::simulate(self),
            RunnerSettings::Async { chunks } => async_runner::simulate(self, chunks),
            RunnerSettings::Glauber {
                maximum_iterations,
                update_rate,
            } => glauber_runner::simulate(self, update_rate, maximum_iterations),
            RunnerSettings::Layered {
                rounds_gap,
                distribution,
            } => layered_runner::simulate(self, rounds_gap, distribution),
        }
    }
}

impl<M, N: Node, R> SimulationRunner<M, N, R>
where
    M: Clone + Send + Sync + 'static,
    N: Send + Sync + 'static,
    N::Settings: Clone + Send,
    N::State: Serialize,
    R: Record
        + serde::Serialize
        + ToSeries
        + for<'a> TryFrom<&'a SimulationState<N>, Error = anyhow::Error>
        + Send
        + Sync
        + 'static,
{
    pub fn simulate_and_subscribe<S>(
        self,
        settings: S::Settings,
    ) -> anyhow::Result<SimulationRunnerHandle<R>>
    where
        S: Subscriber<Record = R> + Send + Sync + 'static,
    {
        let handle = self.simulate()?;
        let mut data_subscriber_handle = handle.subscribe::<S>(settings)?;
        let mut runtime_subscriber_handle =
            handle.subscribe::<RuntimeSubscriber<R>>(Default::default())?;
        let mut settings_subscriber_handle =
            handle.subscribe::<SettingsSubscriber<R>>(Default::default())?;
        std::thread::scope(|s| {
            s.spawn(move || {
                data_subscriber_handle.run();
            });

            s.spawn(move || {
                runtime_subscriber_handle.run();
            });

            s.spawn(move || {
                settings_subscriber_handle.run();
            });
        });

        Ok(handle)
    }
}
