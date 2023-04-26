// std
// crates
use futures::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
// internal
use crate::vote::Tally;

#[derive(Serialize, Deserialize)]
pub struct MockVote {
    view: i64,
}

impl MockVote {
    pub fn view(&self) -> i64 {
        self.view
    }
}

#[allow(dead_code)]
pub struct MockQc {
    count_votes: usize,
}

pub struct Error(String);

#[derive(Clone, Debug)]
pub struct MockTallySettings {
    pub threshold: usize,
}

#[derive(Debug)]
pub struct MockTally {
    threshold: usize,
}

impl MockQc {
    pub fn new(count_votes: usize) -> Self {
        Self { count_votes }
    }

    pub fn votes(&self) -> usize {
        self.count_votes
    }
}

#[async_trait::async_trait]
impl Tally for MockTally {
    type Vote = MockVote;
    type Qc = MockQc;
    type Outcome = ();
    type TallyError = Error;
    type Settings = MockTallySettings;

    fn new(settings: Self::Settings) -> Self {
        let Self::Settings { threshold } = settings;
        Self { threshold }
    }

    async fn tally<S: Stream<Item = Self::Vote> + Unpin + Send>(
        &self,
        view: consensus_engine::View,
        mut vote_stream: S,
    ) -> Result<(Self::Qc, Self::Outcome), Self::TallyError> {
        let mut count_votes = 0;
        while let Some(vote) = vote_stream.next().await {
            if vote.view() != view {
                return Err(Error("Invalid vote".into()));
            }
            count_votes += 1;
            if count_votes > self.threshold {
                return Ok((MockQc { count_votes }, ()));
            }
        }
        Err(Error("Not enough votes".into()))
    }
}
