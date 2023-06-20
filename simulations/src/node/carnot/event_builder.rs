use crate::node::carnot::messages::CarnotMessage;
use crate::util::parse_idx;
use consensus_engine::{Carnot, NewView, Overlay, Qc, StandardQc, Timeout, TimeoutQc, View, Vote};
use nomos_consensus::network::messages::{NewViewMsg, TimeoutMsg, VoteMsg};
use nomos_consensus::NodeId;
use nomos_core::block::Block;
use std::collections::{HashMap, HashSet};
use std::hash::Hash;

pub type CarnotTx = [u8; 32];

pub(crate) struct EventBuilder {
    id: NodeId,
    leader_vote_message: Tally<VoteMsg>,
    vote_message: Tally<VoteMsg>,
    timeout_message: Tally<TimeoutMsg>,
    new_view_message: Tally<NewViewMsg>,
    pub(crate) current_view: View,
}

impl EventBuilder {
    pub fn new(id: NodeId) -> Self {
        Self {
            vote_message: Default::default(),
            leader_vote_message: Default::default(),
            timeout_message: Default::default(),
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
            tracing::debug!(node = parse_idx(&self.id), "voting genesis",);
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
                    tracing::debug!(
                        node=parse_idx(&self.id),
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
                    let voter = msg.voter;
                    let is_next_view_leader = engine.is_next_leader();
                    let is_message_from_root_committee =
                        engine.overlay().is_member_of_root_committee(voter);

                    let tally = if is_message_from_root_committee {
                        &mut self.leader_vote_message
                    } else {
                        &mut self.vote_message
                    };

                    let Some(qc) = msg.qc.clone() else {
                        tracing::warn!(node=?parse_idx(&self.id), current_view = engine.current_view(), "received vote without QC");
                        continue;
                    };

                    // if the message comes from the root committee, then use the leader threshold, otherwise use the leaf threshold
                    let threshold = if is_message_from_root_committee {
                        engine.leader_super_majority_threshold()
                    } else {
                        engine.super_majority_threshold()
                    };

                    if let Some(votes) = tally.tally_by(msg_view, msg, threshold) {
                        if let Some(block) = engine
                            .blocks_in_view(msg_view)
                            .iter()
                            .find(|block| block.id == block_id)
                            .cloned()
                        {
                            tracing::debug!(
                                node=parse_idx(&self.id),
                                votes=votes.len(),
                                current_view = engine.current_view(),
                                block_view=block.view,
                                block=?block.id,
                                "approve block",
                            );

                            if is_next_view_leader && is_message_from_root_committee {
                                events.push(Event::ProposeBlock {
                                    qc: Qc::Standard(StandardQc {
                                        view: block.view,
                                        id: block.id,
                                    }),
                                });
                            } else {
                                events.push(Event::Approve {
                                    qc,
                                    block,
                                    votes: votes.into_iter().map(|v| v.vote).collect(),
                                });
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
                    let threshold = if engine.is_next_leader() {
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
        let entries = self.cache.entry(view).or_default();
        entries.insert(message);
        let entries = entries.len();
        if entries == threshold {
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
    ProposeBlock {
        qc: Qc,
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
    None,
}
