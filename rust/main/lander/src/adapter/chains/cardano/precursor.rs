//! Cardano transaction precursor for the lander adapter.
//!
//! This module defines the intermediate state for building Cardano transactions
//! to process Hyperlane messages.

use serde::{Deserialize, Serialize};

use hyperlane_core::H512;

use crate::transaction::{Transaction, VmSpecificTxData};

/// Precursor data for building a Cardano transaction.
///
/// Contains all the information needed to build and submit a Process transaction
/// to deliver a Hyperlane message to Cardano.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct CardanoTxPrecursor {
    /// The encoded HyperlaneMessage
    pub message_bytes: Vec<u8>,
    /// The ISM metadata (signatures, merkle proof, etc.)
    pub metadata: Vec<u8>,
    /// Fee estimate in lovelace (optional, set after estimation)
    pub fee_estimate: Option<u64>,
    /// Transaction hash after submission (optional)
    pub tx_hash: Option<H512>,
}

impl CardanoTxPrecursor {
    pub fn new(message_bytes: Vec<u8>, metadata: Vec<u8>) -> Self {
        Self {
            message_bytes,
            metadata,
            fee_estimate: None,
            tx_hash: None,
        }
    }
}

/// Trait for accessing CardanoTxPrecursor from a Transaction
pub trait Precursor {
    fn precursor(&self) -> &CardanoTxPrecursor;
    fn precursor_mut(&mut self) -> &mut CardanoTxPrecursor;
}

#[allow(clippy::panic)]
impl Precursor for Transaction {
    fn precursor(&self) -> &CardanoTxPrecursor {
        match &self.vm_specific_data {
            VmSpecificTxData::Cardano(precursor) => precursor,
            _ => panic!("Expected Cardano transaction data"),
        }
    }

    fn precursor_mut(&mut self) -> &mut CardanoTxPrecursor {
        match &mut self.vm_specific_data {
            VmSpecificTxData::Cardano(precursor) => precursor,
            _ => panic!("Expected Cardano transaction data"),
        }
    }
}

/// Calldata structure for Cardano transactions, used for serialization.
///
/// This mirrors the format expected by the hyperlane-cardano crate's process function.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CardanoTxCalldata {
    /// Serialized HyperlaneMessage
    pub message: Vec<u8>,
    /// ISM metadata (merkle proof + signatures)
    pub metadata: Vec<u8>,
}

impl From<CardanoTxCalldata> for CardanoTxPrecursor {
    fn from(value: CardanoTxCalldata) -> Self {
        Self {
            message_bytes: value.message,
            metadata: value.metadata,
            fee_estimate: None,
            tx_hash: None,
        }
    }
}
