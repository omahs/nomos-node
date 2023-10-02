use consensus_engine::View;
use fraction::{Fraction, One};
use full_replication::{AbsoluteNumber, Attestation, Certificate, FullReplication};
use futures::stream::{self, StreamExt};
use nomos_core::{
    da::{certificate::Certificate as _, DaProtocol},
    wire,
};
use nomos_mempool::backend::Status;
use std::{collections::HashSet, time::Duration};
use tests::{MixNode, Node, NomosNode, SpawnConfig};

const TARGET_VIEW: View = View::new(20);
const TIMEOUT_SECS: u64 = 20;

async fn happy_test(nodes: Vec<NomosNode>) {
    let timeout = std::time::Duration::from_secs(TIMEOUT_SECS);
    let timeout = tokio::time::sleep(timeout);
    tokio::select! {
        _ = timeout => panic!("timed out waiting for nodes to reach view {}", TARGET_VIEW),
        _ = async { while stream::iter(&nodes)
            .any(|n| async move { n.consensus_info().await.current_view < TARGET_VIEW })
            .await
        {
            println!(
                "waiting... {}",
                stream::iter(&nodes)
                    .then(|n| async move { format!("{}", n.consensus_info().await.current_view) })
                    .collect::<Vec<_>>()
                    .await
                    .join(" | ")
            );
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        } => {}
    };

    let infos = stream::iter(nodes)
        .then(|n| async move { n.consensus_info().await })
        .collect::<Vec<_>>()
        .await;
    // check that they have the same block
    let blocks = infos
        .iter()
        .map(|i| {
            i.safe_blocks
                .values()
                .find(|b| b.view == TARGET_VIEW)
                .unwrap()
        })
        .collect::<HashSet<_>>();
    assert_eq!(blocks.len(), 1);
}

#[tokio::test]
async fn two_nodes_happy() {
    let (_mixnodes, mixnet_node_configs, mixnet_topology) = MixNode::spawn_nodes(2).await;
    let nodes = NomosNode::spawn_nodes(SpawnConfig::Star {
        n_participants: 2,
        threshold: Fraction::one(),
        timeout: Duration::from_secs(10),
        mixnet_node_configs,
        mixnet_topology,
    })
    .await;
    happy_test(nodes).await;
}

#[tokio::test]
async fn ten_nodes_happy() {
    let (_mixnodes, mixnet_node_configs, mixnet_topology) = MixNode::spawn_nodes(3).await;
    let nodes = NomosNode::spawn_nodes(SpawnConfig::Star {
        n_participants: 10,
        threshold: Fraction::one(),
        timeout: Duration::from_secs(10),
        mixnet_node_configs,
        mixnet_topology,
    })
    .await;
    happy_test(nodes).await;
}

#[tokio::test]
async fn ten_nodes_block_production() {
    let (_mixnodes, mixnet_node_configs, mixnet_topology) = MixNode::spawn_nodes(3).await;
    let nodes = NomosNode::spawn_nodes(SpawnConfig::Star {
        n_participants: 10,
        threshold: Fraction::one(),
        timeout: Duration::from_secs(10),
        mixnet_node_configs,
        mixnet_topology,
    })
    .await;

    let cert = get_dummy_cert();
    let id = cert.hash();
    // TODO: check that the certificate is shared with other nodes
    nodes[0]
        .send_mempool_item(
            tests::nodes::nomos::Pool::Da,
            wire::serialize(&cert).unwrap().into(),
        )
        .await;

    let timeout = std::time::Duration::from_secs(TIMEOUT_SECS);
    tokio::time::timeout(timeout, async move {
        matches!(
            nodes[1]
                .mempoool_status(tests::nodes::nomos::Pool::Da, vec![id])
                .await[0],
            Status::InBlock { .. }
        )
    })
    .await
    .unwrap();
}

fn get_dummy_cert() -> Certificate {
    let mut da =
        <FullReplication<AbsoluteNumber<Attestation, Certificate>>>::new(AbsoluteNumber::new(1));
    let attestation = da.attest(&da.encode(&[])[0]);
    da.recv_attestation(attestation);
    da.certify_dispersal().unwrap()
}
