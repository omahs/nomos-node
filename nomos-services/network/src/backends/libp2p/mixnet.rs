use std::{ops::Range, time::Duration};

use mixnet_client::MixnetClient;
use nomos_core::wire;
use rand::{rngs::OsRng, thread_rng, Rng};
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

use super::{command::Topic, Command, Libp2pConfig};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MixnetMessage {
    pub topic: Topic,
    pub message: Box<[u8]>,
}

impl MixnetMessage {
    pub fn as_bytes(&self) -> Vec<u8> {
        wire::serialize(self).expect("Couldn't serialize MixnetMessage")
    }
    pub fn from_bytes(data: &[u8]) -> Result<Self, wire::Error> {
        wire::deserialize(data)
    }
}

pub fn random_delay(range: &Range<Duration>) -> Duration {
    if range.start == range.end {
        return range.start;
    }
    thread_rng().gen_range(range.start, range.end)
}

pub struct MixnetHandler {
    client: MixnetClient<OsRng>,
    commands_tx: mpsc::Sender<Command>,
}

impl MixnetHandler {
    pub fn new(config: &Libp2pConfig, commands_tx: mpsc::Sender<Command>) -> Self {
        let client = MixnetClient::new(config.mixnet_client.clone(), OsRng);

        Self {
            client,
            commands_tx,
        }
    }

    pub async fn run(&mut self) {
        let Ok(mut stream) = self.client.run().await else {
            tracing::error!("Could not quickstart mixnet stream");
            return;
        };

        while let Some(result) = stream.next().await {
            match result {
                Ok(msg) => {
                    tracing::debug!("receiving message from mixnet client");
                    let Ok(MixnetMessage { topic, message }) = MixnetMessage::from_bytes(&msg)
                    else {
                        tracing::error!("failed to deserialize msg received from mixnet client");
                        continue;
                    };

                    self.commands_tx
                        .send(Command::DirectBroadcastAndRetry {
                            topic,
                            message,
                            retry_count: 0,
                        })
                        .await
                        .unwrap_or_else(|_| tracing::error!("could not schedule broadcast"));
                }
                Err(e) => {
                    todo!("Handle mixclient error: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::random_delay;

    #[test]
    fn test_random_delay() {
        assert_eq!(
            random_delay(&(Duration::ZERO..Duration::ZERO)),
            Duration::ZERO
        );

        let range = Duration::from_millis(10)..Duration::from_millis(100);
        let delay = random_delay(&range);
        assert!(range.start <= delay && delay < range.end);
    }
}
