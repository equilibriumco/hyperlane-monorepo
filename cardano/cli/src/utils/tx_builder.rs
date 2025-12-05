//! Transaction building utilities using pallas-txbuilder

use anyhow::{anyhow, Result};
use pallas_addresses::Network;
use pallas_crypto::hash::Hash;
use pallas_txbuilder::{
    BuildConway, BuiltTransaction, ExUnits, Input, Output, ScriptKind, StagingTransaction,
};

use super::blockfrost::BlockfrostClient;
use super::cbor::{build_mint_redeemer, build_registry_datum, build_registry_admin_register_redeemer, RegistrationData};
use super::crypto::Keypair;
use super::types::Utxo;

/// Parse a UTXO reference string (format: "txhash#index")
fn parse_utxo_ref(s: &str) -> Result<(String, u32)> {
    let parts: Vec<&str> = s.split('#').collect();
    if parts.len() != 2 {
        return Err(anyhow!(
            "Invalid UTXO reference format. Expected 'txhash#index', got '{}'",
            s
        ));
    }
    let tx_hash = parts[0].to_string();
    let output_index: u32 = parts[1]
        .parse()
        .map_err(|_| anyhow!("Invalid output index: {}", parts[1]))?;
    Ok((tx_hash, output_index))
}

/// Transaction builder for Hyperlane Cardano operations
pub struct HyperlaneTxBuilder<'a> {
    client: &'a BlockfrostClient,
    network: Network,
}

impl<'a> HyperlaneTxBuilder<'a> {
    pub fn new(client: &'a BlockfrostClient, network: Network) -> Self {
        Self { client, network }
    }

    /// Build an initialization transaction that mints a state NFT and creates
    /// an initial UTXO at a script address with inline datum
    ///
    /// # Arguments
    /// * `asset_name` - Optional asset name for the NFT (e.g., "Mailbox State"). If None, uses empty name.
    pub async fn build_init_tx(
        &self,
        payer: &Keypair,
        input_utxo: &Utxo,
        collateral_utxo: &Utxo,
        mint_script_cbor: &[u8],
        script_address: &str,
        datum_cbor: &[u8],
        output_lovelace: u64,
        asset_name: Option<&str>,
    ) -> Result<BuiltTransaction> {
        // Get current slot for validity
        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200; // ~2 hours

        // Get PlutusV3 cost model for script data hash
        let cost_model = self.client.get_plutusv3_cost_model().await?;

        // Calculate policy ID from script (PlutusV3 uses tag 0x03)
        let policy_id = super::crypto::script_hash(mint_script_cbor);

        // Convert asset name to bytes (empty vec if None)
        let asset_name_bytes: Vec<u8> = asset_name.map(|n| n.as_bytes().to_vec()).unwrap_or_default();

        // Parse tx hashes
        let input_tx_hash: [u8; 32] = hex::decode(&input_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid input tx hash"))?;

        let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid collateral tx hash"))?;

        let payer_address = payer.address_bech32(self.network);

        // Build inputs
        let input = Input::new(Hash::new(input_tx_hash), input_utxo.output_index as u64);
        let collateral = Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64);

        // Build the script output
        let script_output = Output::new(
            pallas_addresses::Address::from_bech32(script_address)
                .map_err(|e| anyhow!("Invalid script address: {}", e))?,
            output_lovelace,
        )
        .set_inline_datum(datum_cbor.to_vec())
        .add_asset(Hash::new(policy_id), asset_name_bytes.clone(), 1)
        .map_err(|e| anyhow!("Failed to add asset: {:?}", e))?;

        // Build change output
        let fee_estimate = 2_000_000u64;
        let change = input_utxo.lovelace.saturating_sub(output_lovelace).saturating_sub(fee_estimate);

        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

        // Build mint redeemer CBOR
        let mint_redeemer = build_mint_redeemer();

        // Build staging transaction
        let mut staging = StagingTransaction::new()
            .input(input)
            .collateral_input(collateral)
            .output(script_output)
            .mint_asset(Hash::new(policy_id), asset_name_bytes, 1)
            .map_err(|e| anyhow!("Failed to add mint: {:?}", e))?
            .script(ScriptKind::PlutusV3, mint_script_cbor.to_vec())
            .add_mint_redeemer(
                Hash::new(policy_id),
                mint_redeemer,
                Some(ExUnits { mem: 500_000, steps: 200_000_000 }),
            )
            .language_view(ScriptKind::PlutusV3, cost_model)
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(if matches!(self.network, Network::Testnet) { 0 } else { 1 });

        if change > 1_000_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        // Add required signer
        let signer_hash: Hash<28> = Hash::new(payer.verification_key_hash());
        staging = staging.disclosed_signer(signer_hash);

        // Build the transaction
        let tx = staging.build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        Ok(tx)
    }

    /// Sign a built transaction
    pub fn sign_tx(&self, tx: BuiltTransaction, payer: &Keypair) -> Result<Vec<u8>> {
        // Get the transaction hash for signing
        let tx_hash_bytes: &[u8] = &tx.tx_hash.0;

        // Sign the transaction hash
        let signature = payer.sign(tx_hash_bytes);

        // Get the public key
        let public_key = payer.pallas_public_key();

        // Add the signature to the built transaction
        let signed = tx.add_signature(public_key.clone(), signature)
            .map_err(|e| anyhow!("Failed to add signature: {:?}", e))?;

        // Return the serialized signed transaction
        Ok(signed.tx_bytes.0.clone())
    }

    /// Build a registry registration transaction
    /// Spends the registry UTXO and creates a new one with updated datum
    pub async fn build_registry_register_tx(
        &self,
        payer: &Keypair,
        input_utxo: &Utxo,          // For fees
        collateral_utxo: &Utxo,     // Collateral
        registry_utxo: &Utxo,       // Existing registry UTXO
        registry_script_cbor: &[u8], // Registry validator script
        existing_registrations: &[RegistrationData],
        new_registration: &RegistrationData,
        owner_pkh: &str,
    ) -> Result<BuiltTransaction> {
        // Get current slot for validity
        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200; // ~2 hours

        // Get PlutusV3 cost model
        let cost_model = self.client.get_plutusv3_cost_model().await?;

        // Calculate script hash
        let _script_hash = super::crypto::script_hash(registry_script_cbor);

        // Build updated registrations list
        // IMPORTANT: The contract prepends the new registration, not appends!
        // Contract does: let new_registrations = [registration, ..datum.registrations]
        let mut all_registrations = vec![new_registration.clone()];
        all_registrations.extend(existing_registrations.iter().cloned());

        // Build new datum with all registrations
        let new_datum = build_registry_datum(&all_registrations, owner_pkh)?;

        // Build redeemer
        let redeemer = build_registry_admin_register_redeemer(new_registration)?;

        // Parse tx hashes
        let input_tx_hash: [u8; 32] = hex::decode(&input_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid input tx hash"))?;

        let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid collateral tx hash"))?;

        let registry_tx_hash: [u8; 32] = hex::decode(&registry_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid registry tx hash"))?;

        let payer_address = payer.address_bech32(self.network);
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

        // Registry script address
        let registry_address = &registry_utxo.address;
        let registry_addr = pallas_addresses::Address::from_bech32(registry_address)
            .map_err(|e| anyhow!("Invalid registry address: {}", e))?;

        // Build inputs
        let fee_input = Input::new(Hash::new(input_tx_hash), input_utxo.output_index as u64);
        let collateral = Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64);
        let registry_input = Input::new(Hash::new(registry_tx_hash), registry_utxo.output_index as u64);

        // Get state NFT from registry UTXO
        let state_nft = registry_utxo.assets.iter()
            .find(|a| a.quantity == 1)
            .ok_or_else(|| anyhow!("Registry UTXO has no state NFT"))?;

        let nft_policy: [u8; 28] = hex::decode(&state_nft.policy_id)?
            .try_into()
            .map_err(|_| anyhow!("Invalid NFT policy ID"))?;

        let nft_asset_name = hex::decode(&state_nft.asset_name).unwrap_or_default();

        // Build registry output with updated datum and same NFT
        let registry_output = Output::new(registry_addr.clone(), registry_utxo.lovelace)
            .set_inline_datum(new_datum)
            .add_asset(Hash::new(nft_policy), nft_asset_name, 1)
            .map_err(|e| anyhow!("Failed to add asset: {:?}", e))?;

        // Fee estimate
        let fee_estimate = 1_000_000u64;
        let change = input_utxo.lovelace.saturating_sub(fee_estimate);

        // Build staging transaction
        let mut staging = StagingTransaction::new()
            .input(fee_input)
            .input(registry_input)
            .collateral_input(collateral)
            .output(registry_output)
            .script(ScriptKind::PlutusV3, registry_script_cbor.to_vec())
            .add_spend_redeemer(
                Input::new(Hash::new(registry_tx_hash), registry_utxo.output_index as u64),
                redeemer,
                Some(ExUnits { mem: 1_000_000, steps: 500_000_000 }),
            )
            .language_view(ScriptKind::PlutusV3, cost_model)
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(if matches!(self.network, Network::Testnet) { 0 } else { 1 });

        if change > 1_000_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        // Add required signer (must be owner)
        let signer_hash: Hash<28> = Hash::new(payer.verification_key_hash());
        staging = staging.disclosed_signer(signer_hash);

        // Build the transaction
        let tx = staging.build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        Ok(tx)
    }

    /// Build a registry unregister transaction
    /// Spends the registry UTXO and creates a new one with the registration removed
    pub async fn build_registry_unregister_tx(
        &self,
        payer: &Keypair,
        input_utxo: &Utxo,
        collateral_utxo: &Utxo,
        registry_utxo: &Utxo,
        registry_script_cbor: &[u8],
        remaining_registrations: &[RegistrationData],
        script_hash_to_remove: &str,
        owner_pkh: &str,
    ) -> Result<BuiltTransaction> {
        use crate::utils::cbor::{build_registry_datum, build_registry_unregister_redeemer};

        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200;
        let cost_model = self.client.get_plutusv3_cost_model().await?;

        // Build new datum with remaining registrations (the one being removed is already filtered out)
        let new_datum = build_registry_datum(remaining_registrations, owner_pkh)?;

        // Build unregister redeemer
        let redeemer = build_registry_unregister_redeemer(script_hash_to_remove)?;

        let input_tx_hash: [u8; 32] = hex::decode(&input_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid input tx hash"))?;

        let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid collateral tx hash"))?;

        let registry_tx_hash: [u8; 32] = hex::decode(&registry_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid registry tx hash"))?;

        let payer_address = payer.address_bech32(self.network);
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

        let registry_address = &registry_utxo.address;
        let registry_addr = pallas_addresses::Address::from_bech32(registry_address)
            .map_err(|e| anyhow!("Invalid registry address: {}", e))?;

        let fee_input = Input::new(Hash::new(input_tx_hash), input_utxo.output_index as u64);
        let collateral = Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64);
        let registry_input = Input::new(Hash::new(registry_tx_hash), registry_utxo.output_index as u64);

        // Get state NFT from registry UTXO
        let state_nft = registry_utxo.assets.iter()
            .find(|a| a.quantity == 1)
            .ok_or_else(|| anyhow!("Registry UTXO has no state NFT"))?;

        let nft_policy: [u8; 28] = hex::decode(&state_nft.policy_id)?
            .try_into()
            .map_err(|_| anyhow!("Invalid NFT policy ID"))?;

        let nft_asset_name = hex::decode(&state_nft.asset_name).unwrap_or_default();

        // Build registry output with updated datum and same NFT
        let registry_output = Output::new(registry_addr.clone(), registry_utxo.lovelace)
            .set_inline_datum(new_datum)
            .add_asset(Hash::new(nft_policy), nft_asset_name, 1)
            .map_err(|e| anyhow!("Failed to add asset: {:?}", e))?;

        let fee_estimate = 1_000_000u64;
        let change = input_utxo.lovelace.saturating_sub(fee_estimate);

        let mut staging = StagingTransaction::new()
            .input(fee_input)
            .input(registry_input)
            .collateral_input(collateral)
            .output(registry_output)
            .script(ScriptKind::PlutusV3, registry_script_cbor.to_vec())
            .add_spend_redeemer(
                Input::new(Hash::new(registry_tx_hash), registry_utxo.output_index as u64),
                redeemer,
                Some(ExUnits { mem: 1_000_000, steps: 500_000_000 }),
            )
            .language_view(ScriptKind::PlutusV3, cost_model)
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(if matches!(self.network, Network::Testnet) { 0 } else { 1 });

        if change > 1_000_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        let signer_hash: Hash<28> = Hash::new(payer.verification_key_hash());
        staging = staging.disclosed_signer(signer_hash);

        let tx = staging.build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        Ok(tx)
    }

    /// Build a transaction that creates a reference script UTXO
    /// The script is stored in the output's reference_script field
    pub async fn build_reference_script_tx(
        &self,
        payer: &Keypair,
        input_utxo: &Utxo,
        script_cbor: &[u8],
        output_lovelace: u64,
    ) -> Result<BuiltTransaction> {
        let payer_address = payer.address_bech32(self.network);
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200; // ~2 hours

        let input_tx_hash: [u8; 32] = hex::decode(&input_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid tx hash"))?;

        let input = Input::new(Hash::new(input_tx_hash), input_utxo.output_index as u64);

        // Build the reference script output
        // The script is attached to the output, not in the witness set
        let ref_script_output = Output::new(payer_addr.clone(), output_lovelace)
            .set_inline_script(ScriptKind::PlutusV3, script_cbor.to_vec());

        // Calculate fee and change
        let fee_estimate = 500_000u64; // Reference script txs are typically larger
        let change = input_utxo.lovelace.saturating_sub(output_lovelace).saturating_sub(fee_estimate);

        let mut staging = StagingTransaction::new()
            .input(input)
            .output(ref_script_output)
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(if matches!(self.network, Network::Testnet) { 0 } else { 1 });

        if change > 1_000_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        let tx = staging.build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        Ok(tx)
    }

    /// Build a recipient initialization transaction with two-UTXO pattern
    ///
    /// This creates:
    /// 1. State UTXO: script_address + state_NFT (empty name) + datum
    /// 2. Reference Script UTXO: payer_address + ref_NFT ("ref") + script attached
    ///
    /// Both NFTs use the same policy ID, enabling the relayer to discover
    /// the reference script UTXO via the registry's reference_script_locator.
    pub async fn build_init_recipient_two_utxo_tx(
        &self,
        payer: &Keypair,
        input_utxo: &Utxo,
        collateral_utxo: &Utxo,
        mint_script_cbor: &[u8],      // state_nft minting policy
        recipient_script_cbor: &[u8], // recipient validator to attach as reference script
        script_address: &str,         // recipient script address
        datum_cbor: &[u8],            // initial datum for state UTXO
        state_output_lovelace: u64,   // ADA for state UTXO
        ref_output_lovelace: u64,     // ADA for reference script UTXO
    ) -> Result<BuiltTransaction> {
        // Get current slot for validity
        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200; // ~2 hours

        // Get PlutusV3 cost model
        let cost_model = self.client.get_plutusv3_cost_model().await?;

        // Calculate policy ID from mint script
        let policy_id = super::crypto::script_hash(mint_script_cbor);

        // Parse tx hashes
        let input_tx_hash: [u8; 32] = hex::decode(&input_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid input tx hash"))?;

        let collateral_tx_hash: [u8; 32] = hex::decode(&collateral_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid collateral tx hash"))?;

        let payer_address = payer.address_bech32(self.network);
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

        // Build inputs
        let input = Input::new(Hash::new(input_tx_hash), input_utxo.output_index as u64);
        let collateral = Input::new(Hash::new(collateral_tx_hash), collateral_utxo.output_index as u64);

        // Asset names: state NFT has empty name, ref NFT has "ref" (726566 in hex)
        let state_asset_name: Vec<u8> = vec![];
        let ref_asset_name: Vec<u8> = b"ref".to_vec(); // "ref" = 0x726566

        // Output 1: State UTXO at script address with state NFT + datum
        let state_output = Output::new(
            pallas_addresses::Address::from_bech32(script_address)
                .map_err(|e| anyhow!("Invalid script address: {}", e))?,
            state_output_lovelace,
        )
        .set_inline_datum(datum_cbor.to_vec())
        .add_asset(Hash::new(policy_id), state_asset_name.clone(), 1)
        .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

        // Output 2: Reference Script UTXO at payer address with ref NFT + script attached
        let ref_script_output = Output::new(payer_addr.clone(), ref_output_lovelace)
            .add_asset(Hash::new(policy_id), ref_asset_name.clone(), 1)
            .map_err(|e| anyhow!("Failed to add ref NFT: {:?}", e))?
            .set_inline_script(ScriptKind::PlutusV3, recipient_script_cbor.to_vec());

        // Build mint redeemer
        let mint_redeemer = build_mint_redeemer();

        // Fee estimate (larger due to reference script)
        let fee_estimate = 3_000_000u64;
        let total_outputs = state_output_lovelace + ref_output_lovelace;
        let change = input_utxo.lovelace.saturating_sub(total_outputs).saturating_sub(fee_estimate);

        // Build staging transaction - mint both NFTs
        let mut staging = StagingTransaction::new()
            .input(input)
            .collateral_input(collateral)
            .output(state_output)
            .output(ref_script_output)
            // Mint state NFT (empty name)
            .mint_asset(Hash::new(policy_id), state_asset_name, 1)
            .map_err(|e| anyhow!("Failed to add state NFT mint: {:?}", e))?
            // Mint ref NFT ("ref")
            .mint_asset(Hash::new(policy_id), ref_asset_name, 1)
            .map_err(|e| anyhow!("Failed to add ref NFT mint: {:?}", e))?
            .script(ScriptKind::PlutusV3, mint_script_cbor.to_vec())
            .add_mint_redeemer(
                Hash::new(policy_id),
                mint_redeemer,
                Some(ExUnits { mem: 1_000_000, steps: 500_000_000 }),
            )
            .language_view(ScriptKind::PlutusV3, cost_model)
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(if matches!(self.network, Network::Testnet) { 0 } else { 1 });

        if change > 1_000_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        // Add required signer
        let signer_hash: Hash<28> = Hash::new(payer.verification_key_hash());
        staging = staging.disclosed_signer(signer_hash);

        // Build the transaction
        let tx = staging.build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        Ok(tx)
    }

    /// Build a deferred message processing transaction
    ///
    /// This transaction:
    /// 1. Spends the message UTXO (with ProcessStoredMessage redeemer)
    /// 2. Spends the recipient state UTXO (with same redeemer)
    /// 3. Burns the message NFT (with BurnMessage redeemer)
    /// 4. Creates updated state UTXO with incremented messages_processed
    ///
    /// This is for the example_deferred_recipient pattern.
    #[allow(clippy::too_many_arguments)]
    pub async fn build_deferred_process_tx(
        &self,
        payer: &Keypair,
        fee_utxo: &Utxo,
        message_utxo: &Utxo,
        state_utxo: &Utxo,
        message_nft_policy: &str,
        message_id: &str, // Asset name (the message ID)
        recipient_redeemer: &[u8],
        nft_burn_redeemer: &[u8],
        new_state_datum: &[u8],
        recipient_ref_script: Option<&str>,
        nft_ref_script: Option<&str>,
    ) -> Result<BuiltTransaction> {
        // Get current slot for validity
        let current_slot = self.client.get_latest_slot().await?;
        let validity_end = current_slot + 7200; // ~2 hours

        // Get PlutusV3 cost model
        let cost_model = self.client.get_plutusv3_cost_model().await?;

        let payer_address = payer.address_bech32(self.network);
        let payer_addr = pallas_addresses::Address::from_bech32(&payer_address)
            .map_err(|e| anyhow!("Invalid payer address: {}", e))?;

        // Parse tx hashes
        let fee_tx_hash: [u8; 32] = hex::decode(&fee_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid fee tx hash"))?;

        let message_tx_hash: [u8; 32] = hex::decode(&message_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid message tx hash"))?;

        let state_tx_hash: [u8; 32] = hex::decode(&state_utxo.tx_hash)?
            .try_into()
            .map_err(|_| anyhow!("Invalid state tx hash"))?;

        let nft_policy: [u8; 28] = hex::decode(message_nft_policy)?
            .try_into()
            .map_err(|_| anyhow!("Invalid NFT policy ID"))?;

        let message_id_bytes = hex::decode(message_id).unwrap_or_default();

        // Build inputs
        let fee_input = Input::new(Hash::new(fee_tx_hash), fee_utxo.output_index as u64);
        let message_input = Input::new(Hash::new(message_tx_hash), message_utxo.output_index as u64);
        let state_input = Input::new(Hash::new(state_tx_hash), state_utxo.output_index as u64);

        // Get state NFT from state UTXO
        let state_nft = state_utxo
            .assets
            .iter()
            .find(|a| a.quantity == 1)
            .ok_or_else(|| anyhow!("State UTXO has no state NFT"))?;

        let state_nft_policy: [u8; 28] = hex::decode(&state_nft.policy_id)?
            .try_into()
            .map_err(|_| anyhow!("Invalid state NFT policy ID"))?;

        let state_nft_asset_name = hex::decode(&state_nft.asset_name).unwrap_or_default();

        // Build state output (continuation) with updated datum
        let state_addr = pallas_addresses::Address::from_bech32(&state_utxo.address)
            .map_err(|e| anyhow!("Invalid state address: {}", e))?;

        let state_output = Output::new(state_addr, state_utxo.lovelace)
            .set_inline_datum(new_state_datum.to_vec())
            .add_asset(Hash::new(state_nft_policy), state_nft_asset_name, 1)
            .map_err(|e| anyhow!("Failed to add state NFT: {:?}", e))?;

        // Fee estimate
        let fee_estimate = 2_000_000u64;
        let total_input = fee_utxo.lovelace + message_utxo.lovelace;
        let change = total_input
            .saturating_sub(fee_estimate);

        // Build staging transaction
        let mut staging = StagingTransaction::new()
            .input(fee_input)
            .input(message_input.clone())
            .input(state_input.clone())
            .output(state_output)
            // Burn the message NFT (-1)
            .mint_asset(Hash::new(nft_policy), message_id_bytes.clone(), -1)
            .map_err(|e| anyhow!("Failed to add burn: {:?}", e))?
            // Add spend redeemers for both script inputs
            .add_spend_redeemer(
                message_input,
                recipient_redeemer.to_vec(),
                Some(ExUnits { mem: 1_000_000, steps: 500_000_000 }),
            )
            .add_spend_redeemer(
                state_input,
                recipient_redeemer.to_vec(),
                Some(ExUnits { mem: 1_000_000, steps: 500_000_000 }),
            )
            // Add mint redeemer for burning
            .add_mint_redeemer(
                Hash::new(nft_policy),
                nft_burn_redeemer.to_vec(),
                Some(ExUnits { mem: 500_000, steps: 200_000_000 }),
            )
            .language_view(ScriptKind::PlutusV3, cost_model)
            .fee(fee_estimate)
            .invalid_from_slot(validity_end)
            .network_id(if matches!(self.network, Network::Testnet) { 0 } else { 1 });

        // Add reference scripts if provided
        if let Some(ref_script) = recipient_ref_script {
            let (ref_tx, ref_idx) = parse_utxo_ref(ref_script)?;
            let ref_tx_hash: [u8; 32] = hex::decode(&ref_tx)?
                .try_into()
                .map_err(|_| anyhow!("Invalid reference script tx hash"))?;
            staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash), ref_idx as u64));
        }

        if let Some(ref_script) = nft_ref_script {
            let (ref_tx, ref_idx) = parse_utxo_ref(ref_script)?;
            let ref_tx_hash: [u8; 32] = hex::decode(&ref_tx)?
                .try_into()
                .map_err(|_| anyhow!("Invalid NFT reference script tx hash"))?;
            staging = staging.reference_input(Input::new(Hash::new(ref_tx_hash), ref_idx as u64));
        }

        // Add change output if there's enough
        if change > 1_500_000 {
            staging = staging.output(Output::new(payer_addr, change));
        }

        // Add required signer
        let signer_hash: Hash<28> = Hash::new(payer.verification_key_hash());
        staging = staging.disclosed_signer(signer_hash);

        // Build the transaction
        let tx = staging
            .build_conway_raw()
            .map_err(|e| anyhow!("Failed to build transaction: {:?}", e))?;

        Ok(tx)
    }
}
