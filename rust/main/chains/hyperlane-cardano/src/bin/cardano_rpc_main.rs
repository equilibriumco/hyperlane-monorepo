use hyperlane_cardano::rpc::CardanoRpc;
use hyperlane_cardano::{
    CardanoMailbox, CardanoMailboxIndexer, CardanoValidatorAnnounce, ConnectionConf,
};
use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneDomain, HyperlaneMessage, Indexer,
    KnownHyperlaneDomain, ValidatorAnnounce, H256,
};
use std::str::FromStr;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ChainResult<()> {
    let cardano_rpc = CardanoRpc::new(&"http://localhost:3000".parse().unwrap());
    let finalized_block_number = cardano_rpc.get_finalized_block_number().await.unwrap();
    let messages = cardano_rpc
        .get_messages_by_block_range(0, 10)
        .await
        .unwrap();
    let merkle_tree = cardano_rpc.get_latest_merkle_tree().await.unwrap();
    println!("{:?}", finalized_block_number);
    println!("{:?}", merkle_tree);
    println!("{:?}", messages);

    let locator = ContractLocator {
        domain: &HyperlaneDomain::Known(KnownHyperlaneDomain::CardanoTest1),
        address: H256::zero(),
    };
    let conf = ConnectionConf {
        url: "http://localhost:3000".parse().unwrap(),
    };
    let mailbox = CardanoMailbox::new(&conf, locator.clone(), Option::None).unwrap();
    let (tree, tip) = mailbox.tree_and_tip(Option::None).await.unwrap();
    println!("{:?}", tree.count());
    println!("{:?}", tree.root());
    println!("{:?}", tree.branch());
    println!("Block tip: {:?}", tip);

    let cardano_mailbox_indexer = CardanoMailboxIndexer::new(&conf, locator.clone()).unwrap();
    let logs = Indexer::<HyperlaneMessage>::fetch_logs_in_range(&cardano_mailbox_indexer, 0..=10)
        .await
        .unwrap();
    println!("{:?}", logs);

    let validator_announce = CardanoValidatorAnnounce::new(&conf, locator.clone());
    let validator_addresses =
        [
            H256::from_str("0x00000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c8")
                .unwrap(),
        ];
    let validator_storage_locations = validator_announce
        .get_announced_storage_locations(&validator_addresses)
        .await
        .unwrap();
    println!("{:?}", validator_storage_locations);

    Ok(())
}
