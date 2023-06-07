use crate::node::carnot::messages::CarnotMessage;
use crate::util::parse_idx;
use consensus_engine::{Carnot, NewView, Overlay, Qc, Timeout, TimeoutQc, View, Vote, StandardQc};
use nomos_consensus::network::messages::{NewViewMsg, TimeoutMsg, VoteMsg};
use nomos_consensus::NodeId;
use nomos_core::block::{Block, BlockId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub type CarnotTx = [u8; 32];

#[derive(Default, Copy, Clone, Serialize, Deserialize)]
pub struct EventBuilderSettings {
    pub votes_threshold: usize,
    pub timeout_threshold: usize,
    pub new_view_threshold: usize,
}

pub(crate) struct EventBuilder {
    id: NodeId,
    blocks: HashMap<BlockId, Block<CarnotTx>>,
    vote_message: Tally<VoteMsg>,
    timeout_message: Tally<TimeoutMsg>,
    new_view_message: Tally<NewViewMsg>,
    pub(crate) config: EventBuilderSettings,
    pub(crate) current_view: View,
}

impl EventBuilder {
    pub fn new(id: NodeId) -> Self {
        Self {
            vote_message: Default::default(),
            timeout_message: Default::default(),
            config: Default::default(),
            blocks: Default::default(),
            new_view_message: Default::default(),
            current_view: View::default(),
            id,
        }
    }

    pub fn step<O: Overlay>(
        &mut self,
        messages: Vec<CarnotMessage>,
        engine: &Carnot<O>,
    ) -> Vec<Event<CarnotTx>> {
        let mut events = Vec::new();
        // only run when the engine is in the genesis view
        if engine.highest_voted_view() == -1
            && engine.overlay().is_member_of_leaf_committee(self.id)
        {
            let genesis = engine.genesis_block();
            events.push(Event::Approve {
                qc: Qc::Standard(StandardQc {
                    view: genesis.view,
                    id: genesis.id,
                }),
                block: genesis,
                votes: HashSet::new(),
            })
        }

        for message in messages {
            match message {
                CarnotMessage::Proposal(msg) => {
                    let block = Block::from_bytes(&msg.chunk);
                    if self.blocks.contains_key(&block.header().id) {
                        continue;
                    }
                    self.blocks.insert(block.header().id, block.clone());
                    tracing::info!(
                        node=parse_idx(&self.id),
                        leader=parse_idx(&engine.leader(block.header().view)),
                        current_view = engine.current_view(),
                        block_view=block.header().view,
                        block=?block.header().id,
                        parent_block=?block.header().parent(),
                        "receive proposal message",
                    );
                    events.push(Event::Proposal { block })
                }
                CarnotMessage::TimeoutQc(msg) => {
                    events.push(Event::TimeoutQc { timeout_qc: msg.qc });
                }
                CarnotMessage::Vote(msg) => {
                    let msg_view = msg.vote.view;
                    let block_id = msg.vote.block;
                    let Some(qc) = msg.qc.clone() else {
                        tracing::warn!(node=?parse_idx(&self.id), current_view = engine.current_view(), "received vote without QC");
                        continue;
                    };

                    let is_leader = engine.is_leader_for_view(msg_view);

                    // if we are the leader, then use the leader threshold, otherwise use the leaf threshold
                    let threshold = if is_leader {
                        engine.leader_super_majority_threshold()
                    } else {
                        engine.super_majority_threshold()
                    };

                    if let Some(votes) = self.vote_message.tally_by(msg_view, msg, threshold) {
                        if let Some(block) =
                            self.blocks
                                .get(&block_id)
                                .cloned()
                                .map(|b| consensus_engine::Block {
                                    id: b.header().id,
                                    view: b.header().view,
                                    parent_qc: b.header().parent_qc.clone(),
                                })
                        {
                            tracing::info!(
                                node=parse_idx(&self.id),
                                leader=parse_idx(&engine.leader(msg_view)),
                                votes=votes.len(),
                                current_view = engine.current_view(),
                                block_view=block.view,
                                block=?block.id,
                                "approve block",
                            );

                            let parent_qc = block.parent_qc.clone();
                            events.push(Event::Approve {
                                qc,
                                block,
                                votes: votes.into_iter().map(|v| v.vote).collect(),
                            });

                            if is_leader {
                                self.propose_new_block(engine, parent_qc, &mut events);
                            }
                        }
                    }
                }
                CarnotMessage::Timeout(msg) => {
                    let msg_view = msg.vote.view;
                    if let Some(timeouts) = self.timeout_message.tally(msg_view, msg) {
                        events.push(Event::RootTimeout {
                            timeouts: timeouts.into_iter().map(|v| v.vote).collect(),
                        })
                    }
                }
                CarnotMessage::NewView(msg) => {
                    let msg_view = msg.vote.view;
                    let timeout_qc = msg.vote.timeout_qc.clone();
                    self.current_view = core::cmp::max(self.current_view, msg_view);
                    // if we are the leader, then use the leader threshold, otherwise use the leaf threshold
                    let threshold = if engine.is_leader_for_view(msg_view) {
                        engine.leader_super_majority_threshold()
                    } else {
                        engine.super_majority_threshold()
                    };

                    if let Some(new_views) =
                        self.new_view_message.tally_by(msg_view, msg, threshold)
                    {
                        events.push(Event::NewView {
                            new_views: new_views.into_iter().map(|v| v.vote).collect(),
                            timeout_qc,
                        })
                    }
                }
            }
        }

        events
    }

    fn propose_new_block<O: Overlay>(
        &mut self,
        engine: &Carnot<O>,
        parent_qc: Qc,
        events: &mut Vec<Event<CarnotTx>>,
    ) {
        let block = Block::new(
            engine.current_view() + 1,
            parent_qc,
            [].into_iter(),
        );

        tracing::info!(
            node = parse_idx(&self.id),
            leader = parse_idx(&engine.leader(engine.current_view())),
            current_view = engine.current_view(),
            block_view = block.header().view,
            block = ?block.header().id,
            parent_block = ?block.header().parent(),
            "propose block"
        );
        events.push(Event::Proposal { block });
    }
}

struct Tally<T: core::hash::Hash + Eq> {
    cache: HashMap<View, HashSet<T>>,
    threshold: usize,
}

impl<T: core::hash::Hash + Eq> Default for Tally<T> {
    fn default() -> Self {
        Self::new(0)
    }
}

impl<T: core::hash::Hash + Eq> Tally<T> {
    fn new(threshold: usize) -> Self {
        Self {
            cache: Default::default(),
            threshold,
        }
    }

    fn tally(&mut self, view: View, message: T) -> Option<HashSet<T>> {
        self.tally_by(view, message, self.threshold)
    }

    fn tally_by(&mut self, view: View, message: T, threshold: usize) -> Option<HashSet<T>> {
        let entries = self
            .cache
            .entry(view)
            .and_modify(|entry| {
                entry.insert(message);
            })
            .or_default()
            .len();

        if entries >= threshold {
            Some(self.cache.remove(&view).unwrap())
        } else {
            None
        }
    }
}

pub enum Event<Tx: Clone + Hash + Eq> {
    Proposal {
        block: Block<Tx>,
    },
    #[allow(dead_code)]
    Approve {
        qc: Qc,
        block: consensus_engine::Block,
        votes: HashSet<Vote>,
    },
    LocalTimeout,
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