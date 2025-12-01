use hyperlane_cardano::{
    BlockfrostProvider, CardanoMailbox, CardanoMailboxIndexer, CardanoNetwork,
    CardanoValidatorAnnounce, ConnectionConf,
};
use hyperlane_core::{
    ChainResult, ContractLocator, HyperlaneDomain, HyperlaneMessage, Indexer,
    KnownHyperlaneDomain, ValidatorAnnounce, H256,
};
use std::str::FromStr;

#[tokio::main(flavor = "current_thread")]
async fn main() -> ChainResult<()> {
    // Demo using Blockfrost provider directly
    let api_key = std::env::var("BLOCKFROST_API_KEY")
        .expect("Set BLOCKFROST_API_KEY environment variable");

    let provider = BlockfrostProvider::new(&api_key, CardanoNetwork::Preprod);

    let latest_block = provider.get_latest_block().await.unwrap();
    println!("Latest block: {:?}", latest_block);

    // Demo using the full connection config
    let locator = ContractLocator {
        domain: &HyperlaneDomain::Known(KnownHyperlaneDomain::CardanoTest1),
        address: H256::zero(),
    };

    let conf = ConnectionConf {
        url: "https://cardano-preprod.blockfrost.io/api/v0".parse().unwrap(),
        api_key: api_key.clone(),
        network: CardanoNetwork::Preprod,
        mailbox_policy_id: "0000000000000000000000000000000000000000000000000000000000".to_string(),
        registry_policy_id: "0000000000000000000000000000000000000000000000000000000000".to_string(),
        ism_policy_id: "0000000000000000000000000000000000000000000000000000000000".to_string(),
        igp_policy_id: "0000000000000000000000000000000000000000000000000000000000".to_string(),
        validator_announce_policy_id: "0000000000000000000000000000000000000000000000000000000000".to_string(),
    };

    // Test mailbox
    match CardanoMailbox::new(&conf, locator.clone(), None) {
        Ok(mailbox) => {
            match mailbox.tree_and_tip(None).await {
                Ok((tree, tip)) => {
                    println!("Tree count: {:?}", tree.count());
                    println!("Tree root: {:?}", tree.root());
                    println!("Block tip: {:?}", tip);
                }
                Err(e) => {
                    println!("Warning: Could not get mailbox state (expected if contracts not deployed): {}", e);
                }
            }
        }
        Err(e) => {
            println!("Warning: Could not create mailbox: {}", e);
        }
    }

    // Test mailbox indexer
    match CardanoMailboxIndexer::new(&conf, locator.clone()) {
        Ok(indexer) => {
            match Indexer::<HyperlaneMessage>::fetch_logs_in_range(&indexer, 0..=10).await {
                Ok(logs) => {
                    println!("Indexed messages: {:?}", logs.len());
                }
                Err(e) => {
                    println!("Warning: Could not fetch logs: {}", e);
                }
            }
        }
        Err(e) => {
            println!("Warning: Could not create indexer: {}", e);
        }
    }

    // Test validator announce
    let validator_announce = CardanoValidatorAnnounce::new(&conf, locator);
    let validator_addresses = [
        H256::from_str("0x00000000000000000000000070997970c51812dc3a010c7d01b50e0d17dc79c8")
            .unwrap(),
    ];
    match validator_announce
        .get_announced_storage_locations(&validator_addresses)
        .await
    {
        Ok(locations) => {
            println!("Validator storage locations: {:?}", locations);
        }
        Err(e) => {
            println!("Warning: Could not get validator locations: {}", e);
        }
    }

    println!("\nCardano integration demo completed successfully!");
    Ok(())
}
