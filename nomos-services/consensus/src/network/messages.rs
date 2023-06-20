// std
// crates
use serde::{Deserialize, Serialize};
// internal
use crate::NodeId;
use consensus_engine::{BlockId, NewView, Qc, Timeout, TimeoutQc, View, Vote};
use nomos_core::wire;

#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq, Hash)]
pub struct ProposalChunkMsg {
    pub chunk: Box<[u8]>,
    pub proposal: BlockId,
    pub view: View,
}

impl ProposalChunkMsg {
    pub fn as_bytes(&self) -> Box<[u8]> {
        wire::serialize(self).unwrap().into_boxed_slice()
    }

    pub fn from_bytes(data: &[u8]) -> Self {
        wire::deserialize(data).unwrap()
    }
}

#[derive(Debug, Eq, PartialEq, Hash, Serialize, Deserialize, Clone)]
pub struct VoteMsg {
    pub voter: NodeId,
    pub vote: Vote,
    pub qc: Option<Qc>,
}

impl VoteMsg {
    pub fn as_bytes(&self) -> Box<[u8]> {
        wire::serialize(self).unwrap().into_boxed_slice()
    }
    pub fn from_bytes(data: &[u8]) -> Self {
        wire::deserialize(data).unwrap()
    }
}

#[derive(Serialize, Deserialize, Hash, PartialEq, Eq, Clone)]
pub struct NewViewMsg {
    pub voter: NodeId,
    pub vote: NewView,
}

impl NewViewMsg {
    pub fn as_bytes(&self) -> Box<[u8]> {
        wire::serialize(self).unwrap().into_boxed_slice()
    }
    pub fn from_bytes(data: &[u8]) -> Self {
        wire::deserialize(data).unwrap()
    }
}

#[derive(Serialize, Deserialize, Hash, PartialEq, Eq, Clone)]
pub struct TimeoutMsg {
    pub voter: NodeId,
    pub vote: Timeout,
}

impl TimeoutMsg {
    pub fn as_bytes(&self) -> Box<[u8]> {
        wire::serialize(self).unwrap().into_boxed_slice()
    }
    pub fn from_bytes(data: &[u8]) -> Self {
        wire::deserialize(data).unwrap()
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Hash, Clone)]
pub struct TimeoutQcMsg {
    pub source: NodeId,
    pub qc: TimeoutQc,
}

impl TimeoutQcMsg {
    pub fn as_bytes(&self) -> Box<[u8]> {
        wire::serialize(self).unwrap().into_boxed_slice()
    }
    pub fn from_bytes(data: &[u8]) -> Self {
        wire::deserialize(data).unwrap()
    }
}
