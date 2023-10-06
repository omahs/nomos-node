use consensus_engine::View;
use fraction::Fraction;
use futures::stream::{self, StreamExt};
use std::collections::HashSet;
use tests::{ConsensusConfig, MixNode, Node, NomosNode, SpawnConfig};

const TARGET_VIEW: View = View::new(20);

#[tokio::test]
async fn ten_nodes_one_down() {
    let (_mixnodes, mixnet_config) = MixNode::spawn_nodes(3).await;
    let mut nodes = NomosNode::spawn_nodes(SpawnConfig::Chain {
        consensus: ConsensusConfig {
            n_participants: 10,
            threshold: Fraction::new(9u32, 10u32),
            timeout: std::time::Duration::from_secs(5),
        },
        mixnet: mixnet_config,
    })
    .await;
    let mut failed_node = nodes.pop().unwrap();
    failed_node.stop();
    let timeout = std::time::Duration::from_secs(120);
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
    let target_blocks = infos
        .iter()
        .map(|i| i.safe_blocks.values().find(|b| b.view == TARGET_VIEW))
        .collect::<HashSet<_>>();
    // Every nodes must have the same target block (Some(block))
    // , or no node must have it (None).
    assert_eq!(target_blocks.len(), 1);

    // If no node has the target block, check that TARGET_VIEW was reached by timeout_qc.
    let target_block = target_blocks.iter().next().unwrap();
    if target_block.is_none() {
        println!("No node has the block with {TARGET_VIEW:?}. Checking timeout_qcs...");

        let timeout_qcs = infos
            .iter()
            .map(|i| i.last_view_timeout_qc.clone())
            .collect::<HashSet<_>>();
        if timeout_qcs.len() > 1 {
            println!("TIMEOUT_QCS: {timeout_qcs:?}");
        }
        assert_eq!(timeout_qcs.len(), 1);

        let timeout_qc = timeout_qcs.iter().next().unwrap().clone();
        assert!(timeout_qc.is_some());
        // NOTE: This check could be failed if other timeout_qc had occured before `infos` were gathered.
        //       But it should be okay as long as the `timeout` is not too short.
        assert_eq!(timeout_qc.unwrap().view(), TARGET_VIEW.prev());
    }
}
