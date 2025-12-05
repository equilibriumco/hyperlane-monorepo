//! Blockfrost API client

use anyhow::{anyhow, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::types::{Asset, ProtocolParams, Utxo};

/// Blockfrost API client
pub struct BlockfrostClient {
    client: Client,
    base_url: String,
    api_key: String,
}

impl BlockfrostClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.to_string(),
            api_key: api_key.to_string(),
        }
    }

    /// Make a GET request to Blockfrost
    async fn get<T: for<'de> Deserialize<'de>>(&self, endpoint: &str) -> Result<T> {
        let url = format!("{}{}", self.base_url, endpoint);
        let response = self
            .client
            .get(&url)
            .header("project_id", &self.api_key)
            .send()
            .await
            .with_context(|| format!("Failed to request {}", endpoint))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("Blockfrost error {}: {}", status, body));
        }

        response
            .json()
            .await
            .with_context(|| format!("Failed to parse response from {}", endpoint))
    }

    /// Make a POST request to Blockfrost
    async fn post_cbor(&self, endpoint: &str, cbor: &[u8]) -> Result<String> {
        let url = format!("{}{}", self.base_url, endpoint);
        let response = self
            .client
            .post(&url)
            .header("project_id", &self.api_key)
            .header("Content-Type", "application/cbor")
            .body(cbor.to_vec())
            .send()
            .await
            .with_context(|| format!("Failed to request {}", endpoint))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(anyhow!("Blockfrost error {}: {}", status, body));
        }

        response
            .text()
            .await
            .with_context(|| "Failed to read response")
    }

    /// Get UTXOs at an address
    pub async fn get_utxos(&self, address: &str) -> Result<Vec<Utxo>> {
        #[derive(Deserialize)]
        struct BlockfrostUtxo {
            tx_hash: String,
            tx_index: u32,
            output_index: Option<u32>,
            amount: Vec<BlockfrostAmount>,
            data_hash: Option<String>,
            inline_datum: Option<serde_json::Value>,
            reference_script_hash: Option<String>,
        }

        #[derive(Deserialize)]
        struct BlockfrostAmount {
            unit: String,
            quantity: String,
        }

        let endpoint = format!("/addresses/{}/utxos", address);
        let utxos: Vec<BlockfrostUtxo> = match self.get(&endpoint).await {
            Ok(u) => u,
            Err(e) => {
                // Empty address returns 404
                if e.to_string().contains("404") {
                    return Ok(vec![]);
                }
                return Err(e);
            }
        };

        Ok(utxos
            .into_iter()
            .map(|u| {
                let output_index = u.output_index.unwrap_or(u.tx_index);
                let lovelace = u
                    .amount
                    .iter()
                    .find(|a| a.unit == "lovelace")
                    .map(|a| a.quantity.parse().unwrap_or(0))
                    .unwrap_or(0);

                let assets = u
                    .amount
                    .iter()
                    .filter(|a| a.unit != "lovelace")
                    .map(|a| {
                        let (policy_id, asset_name) = if a.unit.len() > 56 {
                            (a.unit[..56].to_string(), a.unit[56..].to_string())
                        } else {
                            (a.unit.clone(), String::new())
                        };
                        Asset {
                            policy_id,
                            asset_name,
                            quantity: a.quantity.parse().unwrap_or(0),
                        }
                    })
                    .collect();

                Utxo {
                    tx_hash: u.tx_hash,
                    output_index,
                    address: address.to_string(),
                    lovelace,
                    assets,
                    datum_hash: u.data_hash,
                    inline_datum: u.inline_datum,
                    reference_script: u.reference_script_hash,
                }
            })
            .collect())
    }

    /// Find UTXO by asset (policy ID + asset name)
    /// Find a UTXO containing an asset with the given policy ID and asset name.
    /// The asset_name should be hex-encoded (e.g., "4d61696c626f78205374617465" for "Mailbox State").
    /// If asset_name is empty, it will search for any asset under the policy and match UTXOs
    /// containing any asset from that policy.
    pub async fn find_utxo_by_asset(&self, policy_id: &str, asset_name: &str) -> Result<Option<Utxo>> {
        #[derive(Deserialize)]
        struct AssetAddress {
            address: String,
            #[allow(dead_code)]
            quantity: String,
        }

        // If asset_name is provided, query directly for that specific asset
        if !asset_name.is_empty() {
            let unit = format!("{}{}", policy_id, asset_name);
            let endpoint = format!("/assets/{}/addresses", unit);
            let addresses: Vec<AssetAddress> = match self.get(&endpoint).await {
                Ok(a) => a,
                Err(e) => {
                    if e.to_string().contains("404") {
                        return Ok(None);
                    }
                    return Err(e);
                }
            };

            // Find the address holding the asset
            for addr in addresses {
                let utxos = self.get_utxos(&addr.address).await?;
                for utxo in utxos {
                    if utxo.assets.iter().any(|a| a.policy_id == policy_id && a.asset_name == asset_name) {
                        return Ok(Some(utxo));
                    }
                }
            }
            return Ok(None);
        }

        // If asset_name is empty, query for assets under this policy
        // Blockfrost API: /assets/policy/{policy_id}
        #[derive(Deserialize)]
        struct PolicyAsset {
            asset: String,
            #[allow(dead_code)]
            quantity: String,
        }

        let endpoint = format!("/assets/policy/{}", policy_id);
        let assets: Vec<PolicyAsset> = match self.get(&endpoint).await {
            Ok(a) => a,
            Err(e) => {
                if e.to_string().contains("404") {
                    return Ok(None);
                }
                return Err(e);
            }
        };

        // For each asset under this policy, try to find a UTXO
        for policy_asset in assets {
            // The asset field is the full unit (policy_id + asset_name_hex)
            let asset_name_from_unit = policy_asset.asset.strip_prefix(policy_id).unwrap_or("");

            let endpoint = format!("/assets/{}/addresses", policy_asset.asset);
            let addresses: Vec<AssetAddress> = match self.get(&endpoint).await {
                Ok(a) => a,
                Err(e) => {
                    if e.to_string().contains("404") {
                        continue;
                    }
                    return Err(e);
                }
            };

            for addr in addresses {
                let utxos = self.get_utxos(&addr.address).await?;
                for utxo in utxos {
                    if utxo.assets.iter().any(|a| a.policy_id == policy_id && a.asset_name == asset_name_from_unit) {
                        return Ok(Some(utxo));
                    }
                }
            }
        }

        Ok(None)
    }

    /// Get latest block slot
    pub async fn get_latest_slot(&self) -> Result<u64> {
        #[derive(Deserialize)]
        struct Block {
            slot: u64,
        }

        let block: Block = self.get("/blocks/latest").await?;
        Ok(block.slot)
    }

    /// Get protocol parameters
    pub async fn get_protocol_params(&self) -> Result<ProtocolParams> {
        #[derive(Deserialize)]
        struct EpochParams {
            min_fee_a: u64,
            min_fee_b: u64,
            coins_per_utxo_size: Option<String>,
            coins_per_utxo_word: Option<String>,
            collateral_percent: u32,
            max_collateral_inputs: u32,
            max_tx_size: u32,
        }

        let params: EpochParams = self.get("/epochs/latest/parameters").await?;

        let coins_per_utxo_byte = params
            .coins_per_utxo_size
            .or(params.coins_per_utxo_word)
            .and_then(|s| s.parse().ok())
            .unwrap_or(4310);

        Ok(ProtocolParams {
            tx_fee_per_byte: params.min_fee_a,
            tx_fee_fixed: params.min_fee_b,
            min_utxo_lovelace: 1_000_000,
            coins_per_utxo_byte,
            collateral_percentage: params.collateral_percent,
            max_collateral_inputs: params.max_collateral_inputs,
            max_tx_size: params.max_tx_size,
        })
    }

    /// Get PlutusV3 cost model as Vec<i64>
    ///
    /// The cost model must be in the canonical Cardano order. This function
    /// returns the cost model values in the order they appear in the Blockfrost
    /// API response, which follows the canonical order.
    pub async fn get_plutusv3_cost_model(&self) -> Result<Vec<i64>> {
        // The cost model keys must be in exact canonical order
        // This ordering is defined by the Cardano Plutus cost model specification
        let canonical_order = [
            "addInteger-cpu-arguments-intercept",
            "addInteger-cpu-arguments-slope",
            "addInteger-memory-arguments-intercept",
            "addInteger-memory-arguments-slope",
            "appendByteString-cpu-arguments-intercept",
            "appendByteString-cpu-arguments-slope",
            "appendByteString-memory-arguments-intercept",
            "appendByteString-memory-arguments-slope",
            "appendString-cpu-arguments-intercept",
            "appendString-cpu-arguments-slope",
            "appendString-memory-arguments-intercept",
            "appendString-memory-arguments-slope",
            "bData-cpu-arguments",
            "bData-memory-arguments",
            "blake2b_256-cpu-arguments-intercept",
            "blake2b_256-cpu-arguments-slope",
            "blake2b_256-memory-arguments",
            "cekApplyCost-exBudgetCPU",
            "cekApplyCost-exBudgetMemory",
            "cekBuiltinCost-exBudgetCPU",
            "cekBuiltinCost-exBudgetMemory",
            "cekConstCost-exBudgetCPU",
            "cekConstCost-exBudgetMemory",
            "cekDelayCost-exBudgetCPU",
            "cekDelayCost-exBudgetMemory",
            "cekForceCost-exBudgetCPU",
            "cekForceCost-exBudgetMemory",
            "cekLamCost-exBudgetCPU",
            "cekLamCost-exBudgetMemory",
            "cekStartupCost-exBudgetCPU",
            "cekStartupCost-exBudgetMemory",
            "cekVarCost-exBudgetCPU",
            "cekVarCost-exBudgetMemory",
            "chooseData-cpu-arguments",
            "chooseData-memory-arguments",
            "chooseList-cpu-arguments",
            "chooseList-memory-arguments",
            "chooseUnit-cpu-arguments",
            "chooseUnit-memory-arguments",
            "consByteString-cpu-arguments-intercept",
            "consByteString-cpu-arguments-slope",
            "consByteString-memory-arguments-intercept",
            "consByteString-memory-arguments-slope",
            "constrData-cpu-arguments",
            "constrData-memory-arguments",
            "decodeUtf8-cpu-arguments-intercept",
            "decodeUtf8-cpu-arguments-slope",
            "decodeUtf8-memory-arguments-intercept",
            "decodeUtf8-memory-arguments-slope",
            "divideInteger-cpu-arguments-constant",
            "divideInteger-cpu-arguments-model-arguments-c00",
            "divideInteger-cpu-arguments-model-arguments-c01",
            "divideInteger-cpu-arguments-model-arguments-c02",
            "divideInteger-cpu-arguments-model-arguments-c10",
            "divideInteger-cpu-arguments-model-arguments-c11",
            "divideInteger-cpu-arguments-model-arguments-c20",
            "divideInteger-cpu-arguments-model-arguments-minimum",
            "divideInteger-memory-arguments-intercept",
            "divideInteger-memory-arguments-minimum",
            "divideInteger-memory-arguments-slope",
            "encodeUtf8-cpu-arguments-intercept",
            "encodeUtf8-cpu-arguments-slope",
            "encodeUtf8-memory-arguments-intercept",
            "encodeUtf8-memory-arguments-slope",
            "equalsByteString-cpu-arguments-constant",
            "equalsByteString-cpu-arguments-intercept",
            "equalsByteString-cpu-arguments-slope",
            "equalsByteString-memory-arguments",
            "equalsData-cpu-arguments-intercept",
            "equalsData-cpu-arguments-slope",
            "equalsData-memory-arguments",
            "equalsInteger-cpu-arguments-intercept",
            "equalsInteger-cpu-arguments-slope",
            "equalsInteger-memory-arguments",
            "equalsString-cpu-arguments-constant",
            "equalsString-cpu-arguments-intercept",
            "equalsString-cpu-arguments-slope",
            "equalsString-memory-arguments",
            "fstPair-cpu-arguments",
            "fstPair-memory-arguments",
            "headList-cpu-arguments",
            "headList-memory-arguments",
            "iData-cpu-arguments",
            "iData-memory-arguments",
            "ifThenElse-cpu-arguments",
            "ifThenElse-memory-arguments",
            "indexByteString-cpu-arguments",
            "indexByteString-memory-arguments",
            "lengthOfByteString-cpu-arguments",
            "lengthOfByteString-memory-arguments",
            "lessThanByteString-cpu-arguments-intercept",
            "lessThanByteString-cpu-arguments-slope",
            "lessThanByteString-memory-arguments",
            "lessThanEqualsByteString-cpu-arguments-intercept",
            "lessThanEqualsByteString-cpu-arguments-slope",
            "lessThanEqualsByteString-memory-arguments",
            "lessThanEqualsInteger-cpu-arguments-intercept",
            "lessThanEqualsInteger-cpu-arguments-slope",
            "lessThanEqualsInteger-memory-arguments",
            "lessThanInteger-cpu-arguments-intercept",
            "lessThanInteger-cpu-arguments-slope",
            "lessThanInteger-memory-arguments",
            "listData-cpu-arguments",
            "listData-memory-arguments",
            "mapData-cpu-arguments",
            "mapData-memory-arguments",
            "mkCons-cpu-arguments",
            "mkCons-memory-arguments",
            "mkNilData-cpu-arguments",
            "mkNilData-memory-arguments",
            "mkNilPairData-cpu-arguments",
            "mkNilPairData-memory-arguments",
            "mkPairData-cpu-arguments",
            "mkPairData-memory-arguments",
            "modInteger-cpu-arguments-constant",
            "modInteger-cpu-arguments-model-arguments-c00",
            "modInteger-cpu-arguments-model-arguments-c01",
            "modInteger-cpu-arguments-model-arguments-c02",
            "modInteger-cpu-arguments-model-arguments-c10",
            "modInteger-cpu-arguments-model-arguments-c11",
            "modInteger-cpu-arguments-model-arguments-c20",
            "modInteger-cpu-arguments-model-arguments-minimum",
            "modInteger-memory-arguments-intercept",
            "modInteger-memory-arguments-slope",
            "multiplyInteger-cpu-arguments-intercept",
            "multiplyInteger-cpu-arguments-slope",
            "multiplyInteger-memory-arguments-intercept",
            "multiplyInteger-memory-arguments-slope",
            "nullList-cpu-arguments",
            "nullList-memory-arguments",
            "quotientInteger-cpu-arguments-constant",
            "quotientInteger-cpu-arguments-model-arguments-c00",
            "quotientInteger-cpu-arguments-model-arguments-c01",
            "quotientInteger-cpu-arguments-model-arguments-c02",
            "quotientInteger-cpu-arguments-model-arguments-c10",
            "quotientInteger-cpu-arguments-model-arguments-c11",
            "quotientInteger-cpu-arguments-model-arguments-c20",
            "quotientInteger-cpu-arguments-model-arguments-minimum",
            "quotientInteger-memory-arguments-intercept",
            "quotientInteger-memory-arguments-minimum",
            "quotientInteger-memory-arguments-slope",
            "remainderInteger-cpu-arguments-constant",
            "remainderInteger-cpu-arguments-model-arguments-c00",
            "remainderInteger-cpu-arguments-model-arguments-c01",
            "remainderInteger-cpu-arguments-model-arguments-c02",
            "remainderInteger-cpu-arguments-model-arguments-c10",
            "remainderInteger-cpu-arguments-model-arguments-c11",
            "remainderInteger-cpu-arguments-model-arguments-c20",
            "remainderInteger-cpu-arguments-model-arguments-minimum",
            "remainderInteger-memory-arguments-intercept",
            "remainderInteger-memory-arguments-minimum",
            "remainderInteger-memory-arguments-slope",
            "serialiseData-cpu-arguments-intercept",
            "serialiseData-cpu-arguments-slope",
            "serialiseData-memory-arguments-intercept",
            "serialiseData-memory-arguments-slope",
            "sha2_256-cpu-arguments-intercept",
            "sha2_256-cpu-arguments-slope",
            "sha2_256-memory-arguments",
            "sha3_256-cpu-arguments-intercept",
            "sha3_256-cpu-arguments-slope",
            "sha3_256-memory-arguments",
            "sliceByteString-cpu-arguments-intercept",
            "sliceByteString-cpu-arguments-slope",
            "sliceByteString-memory-arguments-intercept",
            "sliceByteString-memory-arguments-slope",
            "sndPair-cpu-arguments",
            "sndPair-memory-arguments",
            "subtractInteger-cpu-arguments-intercept",
            "subtractInteger-cpu-arguments-slope",
            "subtractInteger-memory-arguments-intercept",
            "subtractInteger-memory-arguments-slope",
            "tailList-cpu-arguments",
            "tailList-memory-arguments",
            "trace-cpu-arguments",
            "trace-memory-arguments",
            "unBData-cpu-arguments",
            "unBData-memory-arguments",
            "unConstrData-cpu-arguments",
            "unConstrData-memory-arguments",
            "unIData-cpu-arguments",
            "unIData-memory-arguments",
            "unListData-cpu-arguments",
            "unListData-memory-arguments",
            "unMapData-cpu-arguments",
            "unMapData-memory-arguments",
            "verifyEcdsaSecp256k1Signature-cpu-arguments",
            "verifyEcdsaSecp256k1Signature-memory-arguments",
            "verifyEd25519Signature-cpu-arguments-intercept",
            "verifyEd25519Signature-cpu-arguments-slope",
            "verifyEd25519Signature-memory-arguments",
            "verifySchnorrSecp256k1Signature-cpu-arguments-intercept",
            "verifySchnorrSecp256k1Signature-cpu-arguments-slope",
            "verifySchnorrSecp256k1Signature-memory-arguments",
            // Conway additions (PlutusV3 specific)
            "cekConstrCost-exBudgetCPU",
            "cekConstrCost-exBudgetMemory",
            "cekCaseCost-exBudgetCPU",
            "cekCaseCost-exBudgetMemory",
            "bls12_381_G1_add-cpu-arguments",
            "bls12_381_G1_add-memory-arguments",
            "bls12_381_G1_compress-cpu-arguments",
            "bls12_381_G1_compress-memory-arguments",
            "bls12_381_G1_equal-cpu-arguments",
            "bls12_381_G1_equal-memory-arguments",
            "bls12_381_G1_hashToGroup-cpu-arguments-intercept",
            "bls12_381_G1_hashToGroup-cpu-arguments-slope",
            "bls12_381_G1_hashToGroup-memory-arguments",
            "bls12_381_G1_neg-cpu-arguments",
            "bls12_381_G1_neg-memory-arguments",
            "bls12_381_G1_scalarMul-cpu-arguments-intercept",
            "bls12_381_G1_scalarMul-cpu-arguments-slope",
            "bls12_381_G1_scalarMul-memory-arguments",
            "bls12_381_G1_uncompress-cpu-arguments",
            "bls12_381_G1_uncompress-memory-arguments",
            "bls12_381_G2_add-cpu-arguments",
            "bls12_381_G2_add-memory-arguments",
            "bls12_381_G2_compress-cpu-arguments",
            "bls12_381_G2_compress-memory-arguments",
            "bls12_381_G2_equal-cpu-arguments",
            "bls12_381_G2_equal-memory-arguments",
            "bls12_381_G2_hashToGroup-cpu-arguments-intercept",
            "bls12_381_G2_hashToGroup-cpu-arguments-slope",
            "bls12_381_G2_hashToGroup-memory-arguments",
            "bls12_381_G2_neg-cpu-arguments",
            "bls12_381_G2_neg-memory-arguments",
            "bls12_381_G2_scalarMul-cpu-arguments-intercept",
            "bls12_381_G2_scalarMul-cpu-arguments-slope",
            "bls12_381_G2_scalarMul-memory-arguments",
            "bls12_381_G2_uncompress-cpu-arguments",
            "bls12_381_G2_uncompress-memory-arguments",
            "bls12_381_finalVerify-cpu-arguments",
            "bls12_381_finalVerify-memory-arguments",
            "bls12_381_millerLoop-cpu-arguments",
            "bls12_381_millerLoop-memory-arguments",
            "bls12_381_mulMlResult-cpu-arguments",
            "bls12_381_mulMlResult-memory-arguments",
            "keccak_256-cpu-arguments-intercept",
            "keccak_256-cpu-arguments-slope",
            "keccak_256-memory-arguments",
            "blake2b_224-cpu-arguments-intercept",
            "blake2b_224-cpu-arguments-slope",
            "blake2b_224-memory-arguments",
            "integerToByteString-cpu-arguments-c0",
            "integerToByteString-cpu-arguments-c1",
            "integerToByteString-cpu-arguments-c2",
            "integerToByteString-memory-arguments-intercept",
            "integerToByteString-memory-arguments-slope",
            "byteStringToInteger-cpu-arguments-c0",
            "byteStringToInteger-cpu-arguments-c1",
            "byteStringToInteger-cpu-arguments-c2",
            "byteStringToInteger-memory-arguments-intercept",
            "byteStringToInteger-memory-arguments-slope",
            "andByteString-cpu-arguments-intercept",
            "andByteString-cpu-arguments-slope1",
            "andByteString-cpu-arguments-slope2",
            "andByteString-memory-arguments-intercept",
            "andByteString-memory-arguments-slope",
            "orByteString-cpu-arguments-intercept",
            "orByteString-cpu-arguments-slope1",
            "orByteString-cpu-arguments-slope2",
            "orByteString-memory-arguments-intercept",
            "orByteString-memory-arguments-slope",
            "xorByteString-cpu-arguments-intercept",
            "xorByteString-cpu-arguments-slope1",
            "xorByteString-cpu-arguments-slope2",
            "xorByteString-memory-arguments-intercept",
            "xorByteString-memory-arguments-slope",
            "complementByteString-cpu-arguments-intercept",
            "complementByteString-cpu-arguments-slope",
            "complementByteString-memory-arguments-intercept",
            "complementByteString-memory-arguments-slope",
            "readBit-cpu-arguments",
            "readBit-memory-arguments",
            "writeBits-cpu-arguments-intercept",
            "writeBits-cpu-arguments-slope",
            "writeBits-memory-arguments-intercept",
            "writeBits-memory-arguments-slope",
            "replicateByte-cpu-arguments-intercept",
            "replicateByte-cpu-arguments-slope",
            "replicateByte-memory-arguments-intercept",
            "replicateByte-memory-arguments-slope",
            "shiftByteString-cpu-arguments-intercept",
            "shiftByteString-cpu-arguments-slope",
            "shiftByteString-memory-arguments-intercept",
            "shiftByteString-memory-arguments-slope",
            "rotateByteString-cpu-arguments-intercept",
            "rotateByteString-cpu-arguments-slope",
            "rotateByteString-memory-arguments-intercept",
            "rotateByteString-memory-arguments-slope",
            "countSetBits-cpu-arguments-intercept",
            "countSetBits-cpu-arguments-slope",
            "countSetBits-memory-arguments",
            "findFirstSetBit-cpu-arguments-intercept",
            "findFirstSetBit-cpu-arguments-slope",
            "findFirstSetBit-memory-arguments",
            "ripemd_160-cpu-arguments-intercept",
            "ripemd_160-cpu-arguments-slope",
            "ripemd_160-memory-arguments",
        ];

        let params: serde_json::Value = self.get("/epochs/latest/parameters").await?;

        let cost_model = params["cost_models"]["PlutusV3"]
            .as_object()
            .ok_or_else(|| anyhow!("PlutusV3 cost model not found"))?;

        // Extract values in canonical order
        let mut costs = Vec::with_capacity(canonical_order.len());
        for key in canonical_order.iter() {
            if let Some(value) = cost_model.get(*key) {
                costs.push(value.as_i64().unwrap_or(0));
            }
        }

        Ok(costs)
    }

    /// Submit a transaction (CBOR bytes)
    pub async fn submit_tx(&self, tx_cbor: &[u8]) -> Result<String> {
        let tx_hash = self.post_cbor("/tx/submit", tx_cbor).await?;
        // Response is the tx hash as a JSON string
        Ok(tx_hash.trim_matches('"').to_string())
    }

    /// Get transaction details
    pub async fn get_tx(&self, tx_hash: &str) -> Result<TxInfo> {
        self.get(&format!("/txs/{}", tx_hash)).await
    }

    /// Get transactions for an address (paginated, returns tx hashes in order)
    pub async fn get_address_transactions(&self, address: &str, count: u32) -> Result<Vec<AddressTx>> {
        let endpoint = format!("/addresses/{}/transactions?count={}&order=desc", address, count);
        match self.get(&endpoint).await {
            Ok(txs) => Ok(txs),
            Err(e) => {
                if e.to_string().contains("404") {
                    return Ok(vec![]);
                }
                Err(e)
            }
        }
    }

    /// Get transaction UTXOs (inputs and outputs)
    pub async fn get_tx_utxos(&self, tx_hash: &str) -> Result<TxUtxos> {
        self.get(&format!("/txs/{}/utxos", tx_hash)).await
    }

    /// Wait for transaction confirmation
    pub async fn wait_for_tx(&self, tx_hash: &str, timeout_secs: u64) -> Result<TxInfo> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);

        loop {
            if start.elapsed() > timeout {
                return Err(anyhow!("Timeout waiting for transaction {}", tx_hash));
            }

            match self.get_tx(tx_hash).await {
                Ok(info) => return Ok(info),
                Err(e) => {
                    if !e.to_string().contains("404") {
                        return Err(e);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Wait for a specific UTXO to appear at an address
    ///
    /// This is more reliable than wait_for_tx because the transaction can be
    /// indexed before the address UTXOs are updated in Blockfrost.
    pub async fn wait_for_utxo(
        &self,
        address: &str,
        tx_hash: &str,
        output_index: u32,
        timeout_secs: u64,
    ) -> Result<Utxo> {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(timeout_secs);
        let utxo_ref = format!("{}#{}", tx_hash, output_index);

        loop {
            if start.elapsed() > timeout {
                return Err(anyhow!(
                    "Timeout waiting for UTXO {} at address {}",
                    utxo_ref,
                    address
                ));
            }

            match self.get_utxos(address).await {
                Ok(utxos) => {
                    if let Some(utxo) = utxos
                        .into_iter()
                        .find(|u| u.tx_hash == tx_hash && u.output_index == output_index)
                    {
                        return Ok(utxo);
                    }
                    // UTXO not found yet, wait and retry
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
                Err(e) => {
                    // Address might have no UTXOs yet (404), retry
                    if !e.to_string().contains("404") {
                        return Err(e);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

}

/// Transaction information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxInfo {
    pub hash: String,
    pub block: String,
    pub block_height: u64,
    pub block_time: u64,
    pub slot: u64,
    pub index: u32,
    pub fees: String,
    pub size: u32,
}


/// Address transaction info
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressTx {
    pub tx_hash: String,
    pub tx_index: u32,
    pub block_height: u64,
    pub block_time: u64,
}

/// Transaction UTXOs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxUtxos {
    pub hash: String,
    pub inputs: Vec<TxUtxoEntry>,
    pub outputs: Vec<TxUtxoEntry>,
}

/// Transaction UTXO entry (input or output)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxUtxoEntry {
    pub address: String,
    pub amount: Vec<TxUtxoAmount>,
    #[serde(default)]
    pub output_index: u32,
    pub data_hash: Option<String>,
    pub inline_datum: Option<serde_json::Value>,
    pub reference_script_hash: Option<String>,
    pub collateral: Option<bool>,
    pub reference: Option<bool>,
}

/// Transaction UTXO amount
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxUtxoAmount {
    pub unit: String,
    pub quantity: String,
}
