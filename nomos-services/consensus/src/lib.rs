pub mod committee_membership;
pub mod leader_selection;
pub mod network;
mod tally;
mod task_manager;

// std
use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::pin::Pin;
use std::time::Duration;
// crates
use bls_signatures::PrivateKey;
pub use consensus_engine::NodeId;
use futures::{Stream, StreamExt};
use leader_selection::UpdateableLeaderSelection;
use serde::Deserialize;
use serde::{de::DeserializeOwned, Serialize};
use serde_with::serde_as;
use tokio::sync::oneshot::Sender;
use tracing::instrument;
// internal
use crate::network::messages::{
    NetworkMessage, NewViewMsg, ProposalChunkMsg, TimeoutMsg, TimeoutQcMsg, VoteMsg,
};
use crate::network::NetworkAdapter;
use crate::tally::{
    happy::CarnotTally, timeout::TimeoutTally, unhappy::NewViewTally, CarnotTallySettings,
};
use consensus_engine::{
    overlay::RandomBeaconState, AggregateQc, BlockId, Carnot, Committee, LeaderProof, NewView,
    Overlay, Payload, Qc, StandardQc, Timeout, TimeoutQc, View, Vote,
};
use task_manager::TaskManager;

use crate::committee_membership::UpdateableCommitteeMembership;
use nomos_core::block::Block;
use nomos_core::fountain::FountainCode;
use nomos_core::tx::Transaction;
use nomos_core::vote::Tally;
use nomos_mempool::{
    backend::MemPool, network::NetworkAdapter as MempoolAdapter, MempoolMsg, MempoolService,
};
use nomos_network::NetworkService;
use overwatch_rs::services::relay::{OutboundRelay, Relay, RelayMessage};
use overwatch_rs::services::{
    handle::ServiceStateHandle,
    state::{NoOperator, NoState},
    ServiceCore, ServiceData, ServiceId,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

fn default_timeout() -> Duration {
    DEFAULT_TIMEOUT
}

// Random seed for each round provided by the protocol
pub type Seed = [u8; 32];

#[derive(Debug, Deserialize, Serialize)]
pub struct CarnotSettings<Fountain: FountainCode, O: Overlay> {
    pub private_key: [u8; 32],
    pub fountain_settings: Fountain::Settings,
    pub overlay_settings: O::Settings,
    #[serde(default = "default_timeout")]
    pub timeout: Duration,
}

impl<Fountain: FountainCode, O: Overlay> Clone for CarnotSettings<Fountain, O> {
    fn clone(&self) -> Self {
        Self {
            private_key: self.private_key,
            fountain_settings: self.fountain_settings.clone(),
            overlay_settings: self.overlay_settings.clone(),
            timeout: self.timeout,
        }
    }
}

impl<Fountain: FountainCode, O: Overlay> CarnotSettings<Fountain, O> {
    #[inline]
    pub const fn new(
        private_key: [u8; 32],
        fountain_settings: Fountain::Settings,
        overlay_settings: O::Settings,
        timeout: Duration,
    ) -> Self {
        Self {
            private_key,
            fountain_settings,
            overlay_settings,
            timeout,
        }
    }
}

pub struct CarnotConsensus<A, P, M, F, O>
where
    F: FountainCode,
    A: NetworkAdapter,
    M: MempoolAdapter<Tx = P::Tx>,
    P: MemPool,
    O: Overlay + Debug,
    P::Tx: Transaction + Debug + 'static,
    <P::Tx as Transaction>::Hash: Debug,
    A::Backend: 'static,
{
    service_state: ServiceStateHandle<Self>,
    // underlying networking backend. We need this so we can relay and check the types properly
    // when implementing ServiceCore for CarnotConsensus
    network_relay: Relay<NetworkService<A::Backend>>,
    mempool_relay: Relay<MempoolService<M, P>>,
    _fountain: std::marker::PhantomData<F>,
    _overlay: std::marker::PhantomData<O>,
}

impl<A, P, M, F, O> ServiceData for CarnotConsensus<A, P, M, F, O>
where
    F: FountainCode,
    A: NetworkAdapter,
    P: MemPool,
    P::Tx: Transaction + Debug,
    <P::Tx as Transaction>::Hash: Debug,
    M: MempoolAdapter<Tx = P::Tx>,
    O: Overlay + Debug,
{
    const SERVICE_ID: ServiceId = "Carnot";
    type Settings = CarnotSettings<F, O>;
    type State = NoState<Self::Settings>;
    type StateOperator = NoOperator<Self::State>;
    type Message = ConsensusMsg;
}

#[async_trait::async_trait]
impl<A, P, M, F, O> ServiceCore for CarnotConsensus<A, P, M, F, O>
where
    F: FountainCode + Clone + Send + Sync + 'static,
    A: NetworkAdapter + Clone + Send + Sync + 'static,
    P: MemPool + Send + Sync + 'static,
    P::Settings: Send + Sync + 'static,
    P::Tx:
        Debug + Clone + Eq + Hash + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    <P::Tx as Transaction>::Hash: Debug + Send + Sync,
    M: MempoolAdapter<Tx = P::Tx> + Send + Sync + 'static,
    O: Overlay + Debug + Send + Sync + 'static,
    O::LeaderSelection: UpdateableLeaderSelection,
    O::CommitteeMembership: UpdateableCommitteeMembership,
{
    fn init(service_state: ServiceStateHandle<Self>) -> Result<Self, overwatch_rs::DynError> {
        let network_relay = service_state.overwatch_handle.relay();
        let mempool_relay = service_state.overwatch_handle.relay();
        Ok(Self {
            service_state,
            network_relay,
            _fountain: Default::default(),
            _overlay: Default::default(),
            mempool_relay,
        })
    }

    async fn run(mut self) -> Result<(), overwatch_rs::DynError> {
        let network_relay: OutboundRelay<_> = self
            .network_relay
            .connect()
            .await
            .expect("Relay connection with NetworkService should succeed");

        let mempool_relay: OutboundRelay<_> = self
            .mempool_relay
            .connect()
            .await
            .expect("Relay connection with MemPoolService should succeed");

        let CarnotSettings {
            private_key,
            fountain_settings,
            overlay_settings,
            timeout,
        } = self.service_state.settings_reader.get_updated_settings();

        let overlay = O::new(overlay_settings);
        let genesis = consensus_engine::Block {
            id: BlockId::zeros(),
            view: View::new(0),
            parent_qc: Qc::Standard(StandardQc::genesis()),
            leader_proof: LeaderProof::LeaderId {
                leader_id: NodeId::new([0; 32]),
            },
        };
        let mut carnot = Carnot::from_genesis(NodeId::new(private_key), genesis, overlay);
        let adapter = A::new(network_relay).await;
        let fountain = F::new(fountain_settings);
        let private_key = PrivateKey::new(private_key);
        let self_committee = carnot.self_committee();
        let leader_committee = [carnot.id()].into_iter().collect::<Committee>();
        let tally_settings = CarnotTallySettings {
            threshold: carnot.super_majority_threshold(),
            participating_nodes: carnot.child_committees().into_iter().flatten().collect(),
        };
        let leader_tally_settings = CarnotTallySettings {
            threshold: carnot.leader_super_majority_threshold(),
            participating_nodes: carnot.root_committee(),
        };

        let mut task_manager = TaskManager::new();

        let genesis_block = carnot.genesis_block();
        Self::process_view_change(
            carnot.clone(),
            genesis_block.view.prev(),
            &mut task_manager,
            adapter.clone(),
            timeout,
        )
        .await;
        // we already have the genesis block, no need to wait for it
        task_manager.push(
            genesis_block.view,
            Self::gather_votes(
                adapter.clone(),
                self_committee.clone(),
                genesis_block.clone(),
                tally_settings.clone(),
            ),
        );

        if carnot.is_next_leader() {
            let network_adapter = adapter.clone();
            task_manager.push(genesis_block.view.next(), async move {
                let Event::Approve { qc, .. } = Self::gather_votes(
                    network_adapter,
                    leader_committee.clone(),
                    genesis_block,
                    leader_tally_settings.clone(),
                )
                .await
                else {
                    unreachable!()
                };
                Event::ProposeBlock { qc }
            });
        }

        loop {
            tokio::select! {
                    Some(event) = task_manager.next() => {
                        carnot = Self::process_carnot_event(
                            carnot,
                            event,
                            &mut task_manager,
                            adapter.clone(),
                            private_key,
                            mempool_relay.clone(),
                            &fountain,
                            timeout,
                        )
                        .await
                    }
                    Some(msg) = self.service_state.inbound_relay.next() => {
                        Self::process_message(&carnot, msg);
                    }
            }
        }
    }
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum Output<Tx: Clone + Eq + Hash> {
    Send(consensus_engine::Send),
    BroadcastTimeoutQc { timeout_qc: TimeoutQc },
    BroadcastProposal { proposal: Block<Tx> },
}

impl<A, P, M, F, O> CarnotConsensus<A, P, M, F, O>
where
    F: FountainCode + Clone + Send + Sync + 'static,
    A: NetworkAdapter + Clone + Send + Sync + 'static,
    P: MemPool + Send + Sync + 'static,
    P::Settings: Send + Sync + 'static,
    P::Tx:
        Debug + Clone + Eq + Hash + Serialize + serde::de::DeserializeOwned + Send + Sync + 'static,
    <P::Tx as Transaction>::Hash: Debug + Send + Sync,
    M: MempoolAdapter<Tx = P::Tx> + Send + Sync + 'static,
    O: Overlay + Debug + Send + Sync + 'static,
    O::LeaderSelection: UpdateableLeaderSelection,
    O::CommitteeMembership: UpdateableCommitteeMembership,
{
    fn process_message(carnot: &Carnot<O>, msg: ConsensusMsg) {
        match msg {
            ConsensusMsg::Info { tx } => {
                let info = CarnotInfo {
                    id: carnot.id(),
                    current_view: carnot.current_view(),
                    highest_voted_view: carnot.highest_voted_view(),
                    local_high_qc: carnot.high_qc(),
                    safe_blocks: carnot.safe_blocks().clone(),
                    last_view_timeout_qc: carnot.last_view_timeout_qc(),
                    committed_blocks: carnot.latest_committed_blocks(),
                };
                tx.send(info).unwrap_or_else(|e| {
                    tracing::error!("Could not send consensus info through channel: {:?}", e)
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_carnot_event(
        mut carnot: Carnot<O>,
        event: Event<P::Tx>,
        task_manager: &mut TaskManager<View, Event<P::Tx>>,
        adapter: A,
        private_key: PrivateKey,
        mempool_relay: OutboundRelay<MempoolMsg<P::Tx>>,
        fountain: &F,
        timeout: Duration,
    ) -> Carnot<O> {
        let mut output = None;
        let prev_view = carnot.current_view();
        match event {
            Event::Proposal { block, stream } => {
                (carnot, output) =
                    Self::process_block(carnot, block, stream, task_manager, adapter.clone()).await;
            }
            Event::Approve { block, .. } => {
                tracing::debug!("approving proposal {:?}", block);
                let (new_carnot, out) = carnot.approve_block(block);
                carnot = new_carnot;
                output = Some(Output::Send::<P::Tx>(out));
            }
            Event::LocalTimeout { view } => {
                tracing::debug!("local timeout");
                let (new_carnot, out) = carnot.local_timeout();
                carnot = new_carnot;
                output = out.map(Output::Send);
                // keep timeout until the situation is resolved
                task_manager.push(view, async move {
                    tokio::time::sleep(timeout).await;
                    Event::LocalTimeout { view }
                });
            }
            Event::NewView {
                timeout_qc,
                new_views,
            } => {
                (carnot, output) = Self::approve_new_view(
                    carnot,
                    timeout_qc,
                    new_views,
                    task_manager,
                    adapter.clone(),
                )
                .await;
            }
            Event::TimeoutQc { timeout_qc } => {
                (carnot, output) =
                    Self::receive_timeout_qc(carnot, timeout_qc, task_manager, adapter.clone())
                        .await;
            }
            Event::RootTimeout { timeouts } => {
                (carnot, output) = Self::process_root_timeout(carnot, timeouts).await;
            }
            Event::ProposeBlock { qc } => {
                output = Self::propose_block(carnot.id(), private_key, qc, mempool_relay).await;
            }
            _ => {}
        }

        let current_view = carnot.current_view();
        if current_view != prev_view {
            Self::process_view_change(
                carnot.clone(),
                prev_view,
                task_manager,
                adapter.clone(),
                timeout,
            )
            .await;
        }

        if let Some(output) = output {
            handle_output(&adapter, fountain, carnot.id(), output).await;
        }

        carnot
    }

    #[instrument(level = "debug", skip(adapter, task_manager, stream))]
    async fn process_block(
        mut carnot: Carnot<O>,
        block: Block<P::Tx>,
        mut stream: Pin<Box<dyn Stream<Item = Block<P::Tx>> + Send>>,
        task_manager: &mut TaskManager<View, Event<P::Tx>>,
        adapter: A,
    ) -> (Carnot<O>, Option<Output<P::Tx>>) {
        tracing::debug!("received proposal {:?}", block);
        if carnot.highest_voted_view() >= block.header().view {
            tracing::debug!("already voted for view {}", block.header().view);
            return (carnot, None);
        }

        let original_block = block;
        let block = original_block.header().clone();
        let self_committee = carnot.self_committee();
        let leader_committee = [carnot.id()].into_iter().collect();

        let tally_settings = CarnotTallySettings {
            threshold: carnot.super_majority_threshold(),
            participating_nodes: carnot.child_committees().into_iter().flatten().collect(),
        };
        let leader_tally_settings = CarnotTallySettings {
            threshold: carnot.leader_super_majority_threshold(),
            // TODO: add children of root committee
            participating_nodes: carnot.root_committee(),
        };

        match carnot.receive_block(block.clone()) {
            Ok(mut new_state) => {
                let new_view = new_state.current_view();
                if new_view != carnot.current_view() {
                    task_manager.push(
                        block.view,
                        Self::gather_votes(
                            adapter.clone(),
                            self_committee,
                            block.clone(),
                            tally_settings,
                        ),
                    );
                    new_state = Self::update_overlay(
                        new_state,
                        |leader_selection| leader_selection.on_new_block_received(&original_block),
                        |committee_membership| {
                            committee_membership.on_new_block_received(&original_block)
                        },
                    );
                } else {
                    task_manager.push(block.view, async move {
                        if let Some(block) = stream.next().await {
                            Event::Proposal { block, stream }
                        } else {
                            Event::None
                        }
                    });
                }
                carnot = new_state;
            }
            Err(_) => tracing::debug!("invalid block {:?}", block),
        }

        if carnot.is_next_leader() {
            task_manager.push(block.view, async move {
                let Event::Approve { qc, .. } =
                    Self::gather_votes(adapter, leader_committee, block, leader_tally_settings)
                        .await
                else {
                    unreachable!()
                };
                Event::ProposeBlock { qc }
            });
        }

        (carnot, None)
    }

    #[instrument(level = "debug", skip(task_manager, adapter))]
    async fn approve_new_view(
        carnot: Carnot<O>,
        timeout_qc: TimeoutQc,
        new_views: HashSet<NewView>,
        task_manager: &mut TaskManager<View, Event<P::Tx>>,
        adapter: A,
    ) -> (Carnot<O>, Option<Output<P::Tx>>) {
        let leader_committee = [carnot.id()].into_iter().collect();
        let leader_tally_settings = CarnotTallySettings {
            threshold: carnot.leader_super_majority_threshold(),
            // TODO: add children of root committee
            participating_nodes: carnot.root_committee(),
        };
        let (new_carnot, out) = carnot.approve_new_view(timeout_qc.clone(), new_views);
        let new_view = timeout_qc.view().next();
        if carnot.is_next_leader() {
            let high_qc = carnot.high_qc();
            task_manager.push(new_view, async move {
                let _votes = Self::gather_new_views(
                    adapter,
                    leader_committee,
                    timeout_qc,
                    leader_tally_settings.clone(),
                )
                .await;
                Event::ProposeBlock {
                    qc: Qc::Aggregated(AggregateQc {
                        high_qc,
                        view: new_view,
                    }),
                }
            });
        }

        (new_carnot, Some(Output::Send(out)))
    }

    #[instrument(level = "debug", skip(task_manager, adapter))]
    async fn receive_timeout_qc(
        carnot: Carnot<O>,
        timeout_qc: TimeoutQc,
        task_manager: &mut TaskManager<View, Event<P::Tx>>,
        adapter: A,
    ) -> (Carnot<O>, Option<Output<P::Tx>>) {
        let mut new_state = carnot.receive_timeout_qc(timeout_qc.clone());
        let self_committee = carnot.self_committee();
        let tally_settings = CarnotTallySettings {
            threshold: carnot.super_majority_threshold(),
            participating_nodes: carnot.child_committees().into_iter().flatten().collect(),
        };
        task_manager.push(
            timeout_qc.view().next(),
            Self::gather_new_views(adapter, self_committee, timeout_qc.clone(), tally_settings),
        );
        if carnot.current_view() != new_state.current_view() {
            new_state = Self::update_overlay(
                new_state,
                |leader_selection| leader_selection.on_timeout_qc_received(&timeout_qc),
                |committee_membership| committee_membership.on_timeout_qc_received(&timeout_qc),
            );
        }
        (new_state, None)
    }

    #[instrument(level = "debug")]
    async fn process_root_timeout(
        carnot: Carnot<O>,
        timeouts: HashSet<Timeout>,
    ) -> (Carnot<O>, Option<Output<P::Tx>>) {
        // we might have received a timeout_qc sent by some other node and advanced the view
        // already, in which case we should ignore the timeout
        if carnot.current_view()
            != timeouts
                .iter()
                .map(|t| t.view)
                .max()
                .unwrap_or(View::new(0))
        {
            return (carnot, None);
        }

        assert!(
            timeouts.iter().all(|t| t.view == carnot.current_view()),
            "{:?} {}",
            timeouts.iter().collect::<Vec<_>>(),
            carnot.current_view(),
        );
        let high_qc = timeouts
            .iter()
            .map(|t| &t.high_qc)
            .chain(std::iter::once(&carnot.high_qc()))
            .max_by_key(|qc| qc.view)
            .expect("empty root committee")
            .clone();
        let mut output = None;
        if carnot.is_member_of_root_committee() {
            let timeout_qc = TimeoutQc::new(carnot.current_view(), high_qc, carnot.id());
            output = Some(Output::BroadcastTimeoutQc { timeout_qc });
        }
        (carnot, output)
    }

    #[instrument(level = "debug", skip(mempool_relay, private_key))]
    async fn propose_block(
        id: NodeId,
        private_key: PrivateKey,
        qc: Qc,
        mempool_relay: OutboundRelay<MempoolMsg<P::Tx>>,
    ) -> Option<Output<P::Tx>> {
        let (reply_channel, rx) = tokio::sync::oneshot::channel();
        let mut output = None;
        mempool_relay
            .send(MempoolMsg::View {
                ancestor_hint: BlockId::zeros(),
                reply_channel,
            })
            .await
            .unwrap_or_else(|(e, _)| eprintln!("Could not get transactions from mempool {e}"));

        match rx.await {
            Ok(txs) => {
                let beacon = RandomBeaconState::generate_happy(qc.view(), &private_key);
                let proposal = Block::new(qc.view().next(), qc, txs, id, beacon);
                output = Some(Output::BroadcastProposal { proposal });
            }
            Err(e) => tracing::error!("Could not fetch txs {e}"),
        }
        output
    }

    async fn process_view_change(
        carnot: Carnot<O>,
        prev_view: View,
        task_manager: &mut TaskManager<View, Event<P::Tx>>,
        adapter: A,
        timeout: Duration,
    ) {
        let current_view = carnot.current_view();
        // First we cancel previous processing view tasks
        task_manager.cancel(prev_view);
        tracing::debug!("Advanced view from {prev_view} to {current_view}");
        // View change!
        task_manager.push(current_view, async move {
            tokio::time::sleep(timeout).await;
            Event::LocalTimeout { view: current_view }
        });
        task_manager.push(
            current_view.next(),
            Self::gather_block(adapter.clone(), current_view.next()),
        );
        task_manager.push(
            current_view,
            Self::gather_timeout_qc(adapter.clone(), current_view),
        );
        if carnot.is_member_of_root_committee() {
            task_manager.push(
                current_view,
                Self::gather_timeout(
                    adapter,
                    carnot.self_committee(),
                    current_view,
                    CarnotTallySettings {
                        threshold: carnot.leader_super_majority_threshold(),
                        participating_nodes: carnot.root_committee(),
                    },
                ),
            );
        }
    }

    async fn gather_timeout_qc(adapter: A, view: consensus_engine::View) -> Event<P::Tx> {
        if let Some(timeout_qc) = adapter
            .timeout_qc_stream(view)
            .await
            .map(|msg| msg.qc)
            .next()
            .await
        {
            Event::TimeoutQc { timeout_qc }
        } else {
            Event::None
        }
    }

    #[instrument(level = "debug", skip(adapter, tally))]
    async fn gather_votes(
        adapter: A,
        committee: Committee,
        block: consensus_engine::Block,
        tally: CarnotTallySettings,
    ) -> Event<P::Tx> {
        let tally = CarnotTally::new(tally);
        let votes_stream = adapter.votes_stream(&committee, block.view, block.id).await;
        match tally.tally(block.clone(), votes_stream).await {
            Ok((qc, votes)) => Event::Approve { qc, votes, block },
            Err(_e) => {
                todo!("Handle tally error {_e}");
            }
        }
    }

    #[instrument(level = "debug", skip(adapter, tally))]
    async fn gather_new_views(
        adapter: A,
        committee: Committee,
        timeout_qc: TimeoutQc,
        tally: CarnotTallySettings,
    ) -> Event<P::Tx> {
        let tally = NewViewTally::new(tally);
        let stream = adapter
            .new_view_stream(&committee, timeout_qc.view().next())
            .await;
        match tally.tally(timeout_qc.clone(), stream).await {
            Ok((_qc, new_views)) => Event::NewView {
                timeout_qc,
                new_views,
            },
            Err(_e) => {
                todo!("Handle tally error {_e}");
            }
        }
    }

    #[instrument(level = "debug", skip(adapter, tally))]
    async fn gather_timeout(
        adapter: A,
        committee: Committee,
        view: consensus_engine::View,
        tally: CarnotTallySettings,
    ) -> Event<P::Tx> {
        let tally = TimeoutTally::new(tally);
        let stream = adapter.timeout_stream(&committee, view).await;
        match tally.tally(view, stream).await {
            Ok((_, timeouts)) => Event::RootTimeout { timeouts },
            Err(_e) => {
                todo!("Handle tally error {_e}");
            }
        }
    }

    #[instrument(level = "debug", skip(adapter))]
    async fn gather_block(adapter: A, view: consensus_engine::View) -> Event<P::Tx> {
        let stream = adapter
            .proposal_chunks_stream(view)
            .await
            .filter_map(move |msg| {
                async move {
                    let proposal = Block::from_bytes(&msg.chunk);
                    if proposal.header().id == msg.proposal {
                        // TODO: Leader is faulty? what should we do?
                        Some(proposal)
                    } else {
                        None
                    }
                }
            });
        let mut stream = Box::pin(stream);
        if let Some(block) = stream.next().await {
            Event::Proposal { block, stream }
        } else {
            Event::None
        }
    }

    fn update_leader_selection<
        E: std::error::Error,
        Fl: FnOnce(O::LeaderSelection) -> Result<O::LeaderSelection, E>,
    >(
        carnot: Carnot<O>,
        leader_selection_f: Fl,
    ) -> Carnot<O> {
        carnot
            .update_overlay(|overlay| overlay.update_leader_selection(leader_selection_f))
            .unwrap()
    }

    fn update_committee_membership<
        E: std::error::Error,
        Fm: FnOnce(O::CommitteeMembership) -> Result<O::CommitteeMembership, E>,
    >(
        carnot: Carnot<O>,
        committee_membership_f: Fm,
    ) -> Carnot<O> {
        carnot
            .update_overlay(|overlay| overlay.update_committees(committee_membership_f))
            .unwrap()
    }

    fn update_overlay<
        El: std::error::Error,
        Em: std::error::Error,
        Fl: FnOnce(O::LeaderSelection) -> Result<O::LeaderSelection, El>,
        Fm: FnOnce(O::CommitteeMembership) -> Result<O::CommitteeMembership, Em>,
    >(
        carnot: Carnot<O>,
        leader_selection_f: Fl,
        committee_membership_f: Fm,
    ) -> Carnot<O> {
        let carnot = Self::update_leader_selection(carnot, leader_selection_f);
        Self::update_committee_membership(carnot, committee_membership_f)
    }
}

async fn handle_output<A, F, Tx>(adapter: &A, fountain: &F, node_id: NodeId, output: Output<Tx>)
where
    A: NetworkAdapter,
    F: FountainCode,
    Tx: Hash + Eq + Clone + Serialize + DeserializeOwned + Debug,
{
    match output {
        Output::Send(consensus_engine::Send { to, payload }) => match payload {
            Payload::Vote(vote) => {
                adapter
                    .send(
                        NetworkMessage::Vote(VoteMsg {
                            voter: node_id,
                            vote,
                            qc: None, // TODO: handle root commmittee members
                        }),
                        &to,
                    )
                    .await;
            }
            Payload::Timeout(timeout) => {
                adapter
                    .send(
                        NetworkMessage::Timeout(TimeoutMsg {
                            voter: node_id,
                            vote: timeout,
                        }),
                        &to,
                    )
                    .await;
            }
            Payload::NewView(new_view) => {
                adapter
                    .send(
                        NetworkMessage::NewView(NewViewMsg {
                            voter: node_id,
                            vote: new_view,
                        }),
                        &to,
                    )
                    .await;
            }
        },
        Output::BroadcastProposal { proposal } => {
            fountain
                .encode(&proposal.as_bytes())
                .for_each(|chunk| {
                    adapter.broadcast(NetworkMessage::ProposalChunk(ProposalChunkMsg {
                        proposal: proposal.header().id,
                        chunk: chunk.to_vec().into_boxed_slice(),
                        view: proposal.header().view,
                    }))
                })
                .await;
        }
        Output::BroadcastTimeoutQc { timeout_qc } => {
            adapter
                .broadcast(NetworkMessage::TimeoutQc(TimeoutQcMsg {
                    source: node_id,
                    qc: timeout_qc,
                }))
                .await;
        }
    }
}

enum Event<Tx: Clone + Hash + Eq> {
    Proposal {
        block: Block<Tx>,
        stream: Pin<Box<dyn Stream<Item = Block<Tx>> + Send>>,
    },
    #[allow(dead_code)]
    Approve {
        qc: Qc,
        block: consensus_engine::Block,
        votes: HashSet<Vote>,
    },
    LocalTimeout {
        view: View,
    },
    NewView {
        timeout_qc: TimeoutQc,
        new_views: HashSet<NewView>,
    },
    TimeoutQc {
        timeout_qc: TimeoutQc,
    },
    RootTimeout {
        timeouts: HashSet<Timeout>,
    },
    ProposeBlock {
        qc: Qc,
    },
    None,
}

#[derive(Debug)]
pub enum ConsensusMsg {
    Info { tx: Sender<CarnotInfo> },
}

impl RelayMessage for ConsensusMsg {}

#[serde_as]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CarnotInfo {
    pub id: NodeId,
    pub current_view: View,
    pub highest_voted_view: View,
    pub local_high_qc: StandardQc,
    #[serde_as(as = "Vec<(_, _)>")]
    pub safe_blocks: HashMap<BlockId, consensus_engine::Block>,
    pub last_view_timeout_qc: Option<TimeoutQc>,
    pub committed_blocks: Vec<BlockId>,
}

#[cfg(test)]
mod tests {
    use consensus_engine::Block;

    use super::*;

    #[test]
    fn serde_carnot_info() {
        let info = CarnotInfo {
            id: NodeId::new([0; 32]),
            current_view: View::new(1),
            highest_voted_view: View::new(-1),
            local_high_qc: StandardQc {
                view: View::new(0),
                id: BlockId::zeros(),
            },
            safe_blocks: HashMap::from([(
                BlockId::zeros(),
                Block {
                    id: BlockId::zeros(),
                    view: View::new(0),
                    parent_qc: Qc::Standard(StandardQc {
                        view: View::new(0),
                        id: BlockId::zeros(),
                    }),
                    leader_proof: LeaderProof::LeaderId {
                        leader_id: NodeId::new([0; 32]),
                    },
                },
            )]),
            last_view_timeout_qc: None,
            committed_blocks: vec![BlockId::zeros()],
        };

        let serialized = serde_json::to_string(&info).unwrap();
        assert_eq!(
            serialized,
            r#"{\"id\":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],\"current_view\":1,\"highest_voted_view\":-1,\"local_high_qc\":{\"view\":0,\"id\":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]},\"safe_blocks\":[[[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0],{\"view\":0,\"parent_qc\":{\"Standard\":{\"view\":0,\"id\":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}},\"leader_proof\":{\"leader_id\":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}}]],\"last_view_timeout_qc\":null,\"committed_blocks\":[[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]]}"#
        );

        let deserialized: CarnotInfo = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, info);
    }
}
