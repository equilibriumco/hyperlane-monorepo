//! `InterchainSecurityModule` + `MultisigIsm` impls for Midnight.
//!
//! Validators, threshold, and module type all come from a single read of the
//! deployed contract's state, so the relayer always signs against the set the
//! on-chain ISM will check and nothing can drift out of config. The contract
//! keeps one validator set per origin domain, so the set is the one of the
//! message's origin.

use async_trait::async_trait;

use hyperlane_core::{
    ChainResult, HyperlaneChain, HyperlaneContract, HyperlaneDomain, HyperlaneMessage,
    HyperlaneProvider, InterchainSecurityModule, Metadata, ModuleType, MultisigIsm, H256, U256,
};

use crate::state_decode::IsmState;
use crate::{HyperlaneMidnightError, MidnightProvider};

/// The WarpRoute only implements `MessageIdMultisig`, so any other value means
/// the deployed contract verifies in a way this build does not support.
fn module_type_from_u8(value: u8) -> ChainResult<ModuleType> {
    if value == ModuleType::MessageIdMultisig as u8 {
        Ok(ModuleType::MessageIdMultisig)
    } else {
        Err(HyperlaneMidnightError::StateDecode(format!(
            "unsupported on-chain ISM module_type {value}: only {} (MessageIdMultisig) is supported",
            ModuleType::MessageIdMultisig as u8
        ))
        .into())
    }
}

/// The validators and threshold the contract checks `message` against: the
/// set of its origin. An origin with no set is an error, since the contract
/// refuses every message from it.
fn validators_and_threshold_for(
    state: &IsmState,
    message: &HyperlaneMessage,
) -> ChainResult<(Vec<H256>, u8)> {
    let set = state.validator_set(message.origin)?;
    // The metadata pipeline wants H256, so left-pad each address with 12
    // zero bytes — the standard `addressToBytes32` convention.
    let padded: Vec<H256> = set
        .validators
        .iter()
        .map(|addr| {
            let mut bytes = [0u8; 32];
            bytes[12..].copy_from_slice(addr);
            H256::from(bytes)
        })
        .collect();
    Ok((padded, set.threshold))
}

/// `InterchainSecurityModule` for Midnight.
#[derive(Debug)]
pub struct MidnightInterchainSecurityModule {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightInterchainSecurityModule {
    /// Construct a new ISM handle.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }
}

impl HyperlaneChain for MidnightInterchainSecurityModule {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightInterchainSecurityModule {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl InterchainSecurityModule for MidnightInterchainSecurityModule {
    async fn module_type(&self) -> ChainResult<ModuleType> {
        let address = format!("{:x}", self.address);
        let state = self.provider.indexer().read_ism_state(&address).await?;
        module_type_from_u8(state.module_type)
    }

    async fn dry_run_verify(
        &self,
        _message: &HyperlaneMessage,
        _metadata: &Metadata,
    ) -> ChainResult<Option<U256>> {
        // Fees are DUST the wallet computes at submission time, so there is
        // nothing to estimate; `None` lets the relayer fall back to its own
        // logic without dry-running.
        Ok(None)
    }
}

/// `MultisigIsm` for Midnight.
#[derive(Debug)]
pub struct MidnightMultisigIsm {
    address: H256,
    domain: HyperlaneDomain,
    provider: MidnightProvider,
}

impl MidnightMultisigIsm {
    /// Validators and threshold are read from chain state on each
    /// `validators_and_threshold` call, not from config.
    pub fn new(address: H256, domain: HyperlaneDomain, provider: MidnightProvider) -> Self {
        Self {
            address,
            domain,
            provider,
        }
    }
}

impl HyperlaneChain for MidnightMultisigIsm {
    fn domain(&self) -> &HyperlaneDomain {
        &self.domain
    }

    fn provider(&self) -> Box<dyn HyperlaneProvider> {
        Box::new(self.provider.clone())
    }
}

impl HyperlaneContract for MidnightMultisigIsm {
    fn address(&self) -> H256 {
        self.address
    }
}

#[async_trait]
impl MultisigIsm for MidnightMultisigIsm {
    async fn validators_and_threshold(
        &self,
        message: &HyperlaneMessage,
    ) -> ChainResult<(Vec<H256>, u8)> {
        let address = format!("{:x}", self.address);
        let state = self.provider.indexer().read_ism_state(&address).await?;
        validators_and_threshold_for(&state, message)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::state_decode::ValidatorSet;

    fn message_from(origin: u32) -> HyperlaneMessage {
        HyperlaneMessage {
            version: 3,
            nonce: 0,
            origin,
            sender: H256::repeat_byte(0x33),
            destination: 1234,
            recipient: H256::repeat_byte(0x44),
            body: vec![0u8; 64],
        }
    }

    fn padded(addr: [u8; 20]) -> H256 {
        let mut bytes = [0u8; 32];
        bytes[12..].copy_from_slice(&addr);
        H256::from(bytes)
    }

    fn two_origin_state() -> IsmState {
        IsmState {
            validator_sets: BTreeMap::from([
                (
                    11155111,
                    ValidatorSet {
                        validators: vec![[0xA1; 20]],
                        threshold: 1,
                        slots: 9,
                    },
                ),
                (
                    31337,
                    ValidatorSet {
                        validators: vec![[0xB1; 20], [0xB2; 20], [0xB3; 20]],
                        threshold: 2,
                        slots: 9,
                    },
                ),
            ]),
            module_type: 5,
        }
    }

    #[test]
    fn validators_and_threshold_follow_the_message_origin() {
        let state = two_origin_state();

        assert_eq!(
            validators_and_threshold_for(&state, &message_from(11155111)).unwrap(),
            (vec![padded([0xA1; 20])], 1),
            "the set of origin 11155111"
        );
        assert_eq!(
            validators_and_threshold_for(&state, &message_from(31337)).unwrap(),
            (
                vec![padded([0xB1; 20]), padded([0xB2; 20]), padded([0xB3; 20])],
                2
            ),
            "the set of origin 31337, in slot order"
        );
    }

    #[test]
    fn validators_and_threshold_error_names_an_origin_without_a_set() {
        let state = two_origin_state();

        let err = validators_and_threshold_for(&state, &message_from(421614))
            .expect_err("no set for origin 421614")
            .to_string();
        assert!(
            err.contains("no validator set for origin domain 421614"),
            "the error names the origin: {err}"
        );
    }
}
