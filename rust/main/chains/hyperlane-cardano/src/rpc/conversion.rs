use hex::FromHex;
use hyperlane_cardano_rpc_rust_client::models::MessagesByBlockRange200ResponseMessagesInnerMessage;
use hyperlane_core::{HyperlaneMessage, H256};
use std::str::FromStr;

pub trait FromRpc<T>: Sized {
    fn from_rpc(input: &T) -> Self;
}

impl FromRpc<MessagesByBlockRange200ResponseMessagesInnerMessage> for HyperlaneMessage {
    fn from_rpc(input: &MessagesByBlockRange200ResponseMessagesInnerMessage) -> Self {
        // TODO[cardano]: better parsing of RPC results.
        HyperlaneMessage {
            version: input.version as u8,
            nonce: input.nonce as u32,
            origin: input.origin_domain as u32,
            sender: H256::from_str(input.sender.as_str()).unwrap(),
            destination: input.destination_domain as u32,
            recipient: H256::from_str(input.recipient.as_str()).unwrap(),
            body: Vec::from_hex(input.body.strip_prefix("0x").unwrap()).unwrap(),
        }
    }
}
