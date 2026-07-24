//! Blockfrost API client

use anyhow::{anyhow, Context, Result};
use pallas_primitives::conway::RedeemerTag;
use pallas_traverse::MultiEraTx;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::types::{Asset, ProtocolParams, Utxo};

/// ExUnits a redeemer needs, as reported by transaction evaluation.
#[derive(Debug)]
pub struct ExUnitsRequired {
    pub mem: u64,
    pub steps: u64,
}

/// Blockfrost proxies Ogmios, whose evaluation payload has shifted shape across
/// versions; accept each rather than pin to one.
fn parse_evaluated_ex_units(
    result: &serde_json::Value,
) -> Result<HashMap<String, ExUnitsRequired>> {
    let mut units = HashMap::new();

    // Ogmios v6: { "result": [ { "validator": {...}, "budget": {...} }, ... ] }
    if let Some(entries) = result.get("result").and_then(|r| r.as_array()) {
        for entry in entries {
            let (Some(validator), Some(budget)) = (entry.get("validator"), entry.get("budget"))
            else {
                continue;
            };
            let purpose = validator
                .get("purpose")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let index = validator.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
            units.insert(
                format!("{purpose}:{index}"),
                ExUnitsRequired {
                    mem: budget.get("memory").and_then(|v| v.as_u64()).unwrap_or(0),
                    steps: budget.get("cpu").and_then(|v| v.as_u64()).unwrap_or(0),
                },
            );
        }
        if !units.is_empty() {
            return Ok(units);
        }
    }

    // Ogmios v5: { "result": { "EvaluationResult": { "spend:0": {...} } } }
    let v5 = result
        .get("result")
        .and_then(|r| r.get("EvaluationResult"))
        .or_else(|| result.get("EvaluationResult"));
    if let Some(entries) = v5.and_then(|r| r.as_object()) {
        for (key, value) in entries {
            units.insert(
                key.clone(),
                ExUnitsRequired {
                    mem: value.get("memory").and_then(|v| v.as_u64()).unwrap_or(0),
                    steps: value.get("steps").and_then(|v| v.as_u64()).unwrap_or(0),
                },
            );
        }
        if !units.is_empty() {
            return Ok(units);
        }
    }

    // A script failed. The payload holds the validator's traces, which is the
    // only place the actual reason is ever spelled out.
    if let Some(failure) = result
        .get("result")
        .and_then(|r| r.get("EvaluationFailure"))
        .or_else(|| result.get("fault"))
    {
        return Err(anyhow!("Script evaluation failed: {failure:#}"));
    }

    Err(anyhow!("Unrecognized evaluation response: {result}"))
}

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

    /// Get UTXOs at an address (handles pagination to fetch all UTXOs)
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

        // Fetch all pages of UTXOs (Blockfrost returns max 100 per page)
        let mut all_utxos: Vec<BlockfrostUtxo> = Vec::new();
        let mut page = 1;
        loop {
            let endpoint = format!("/addresses/{}/utxos?page={}&count=100", address, page);
            let utxos: Vec<BlockfrostUtxo> = match self.get(&endpoint).await {
                Ok(u) => u,
                Err(e) => {
                    // Empty address returns 404
                    if e.to_string().contains("404") {
                        break;
                    }
                    // If this is page 1, return the error; otherwise just stop paginating
                    if page == 1 {
                        return Err(e);
                    }
                    break;
                }
            };

            let count = utxos.len();
            all_utxos.extend(utxos);

            // If we got fewer than 100, we've reached the last page
            if count < 100 {
                break;
            }
            page += 1;
        }

        let utxos = all_utxos;

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
    pub async fn find_utxo_by_asset(
        &self,
        policy_id: &str,
        asset_name: &str,
    ) -> Result<Option<Utxo>> {
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
                    if utxo
                        .assets
                        .iter()
                        .any(|a| a.policy_id == policy_id && a.asset_name == asset_name)
                    {
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
                    if utxo
                        .assets
                        .iter()
                        .any(|a| a.policy_id == policy_id && a.asset_name == asset_name_from_unit)
                    {
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

    /// Get the PlutusV3 cost model in canonical ledger order.
    ///
    /// Uses Blockfrost's `cost_models_raw`, which is already a canonically
    /// ordered array. The named `cost_models` object must not be used: its key
    /// set grows at each hard fork (350 params at protocol 11.0, up from 298),
    /// and any missing entry silently yields a wrong script data hash.
    pub async fn get_plutusv3_cost_model(&self) -> Result<Vec<i64>> {
        let params: serde_json::Value = self.get("/epochs/latest/parameters").await?;

        let raw = params["cost_models_raw"]["PlutusV3"]
            .as_array()
            .ok_or_else(|| anyhow!("PlutusV3 cost model not found in cost_models_raw"))?;

        raw.iter()
            .map(|v| {
                v.as_i64()
                    .ok_or_else(|| anyhow!("non-integer PlutusV3 cost model entry: {v}"))
            })
            .collect()
    }

    /// Run the transaction's scripts without submitting it.
    ///
    /// Returns the ExUnits each redeemer actually needs, keyed by
    /// `"<purpose>:<index>"` (e.g. `"spend:0"`). A script that fails here comes
    /// back as an `Err` carrying the validator's own trace output — the same
    /// failure at submit time is reported by the node as an opaque
    /// `ValidationTagMismatch` with no logs at all.
    pub async fn evaluate_tx(&self, tx_cbor: &[u8]) -> Result<HashMap<String, ExUnitsRequired>> {
        // This endpoint wants the CBOR hex-encoded, unlike /tx/submit which
        // takes the raw bytes.
        let url = format!("{}/utils/txs/evaluate", self.base_url);
        let response = self
            .client
            .post(&url)
            .header("project_id", &self.api_key)
            .header("Content-Type", "application/cbor")
            .body(hex::encode(tx_cbor))
            .send()
            .await
            .with_context(|| "Failed to request /utils/txs/evaluate")?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("Blockfrost error {}: {}", status, body));
        }

        let json: serde_json::Value =
            serde_json::from_str(&body).with_context(|| "Failed to parse evaluate response")?;
        parse_evaluated_ex_units(&json)
    }

    /// Evaluate before submitting so script failures surface with their traces,
    /// and so a redeemer whose declared budget is too small is named outright
    /// rather than failing as an unexplained node rejection.
    async fn preflight(&self, tx_cbor: &[u8]) -> Result<()> {
        let tx = MultiEraTx::decode(tx_cbor)
            .map_err(|e| anyhow!("Failed to decode transaction for preflight: {e}"))?;

        // Nothing to evaluate on a script-free transaction, and evaluating one
        // returns an empty result that reads as an unparseable response.
        let redeemers = tx.redeemers();
        if redeemers.is_empty() {
            return Ok(());
        }

        let required = self.evaluate_tx(tx_cbor).await?;

        for redeemer in redeemers {
            let purpose = match redeemer.tag() {
                RedeemerTag::Spend => "spend",
                RedeemerTag::Mint => "mint",
                RedeemerTag::Cert => "certificate",
                RedeemerTag::Reward => "withdrawal",
                RedeemerTag::Vote => "vote",
                RedeemerTag::Propose => "propose",
            };
            let key = format!("{}:{}", purpose, redeemer.index());
            let Some(needed) = required.get(&key) else {
                continue;
            };
            let declared = redeemer.ex_units();
            if declared.mem < needed.mem || declared.steps < needed.steps {
                return Err(anyhow!(
                    "Redeemer {key} is under-budgeted: declared mem={} steps={}, needs mem={} steps={}. \
                     Raise the ExUnits declared for this redeemer where the transaction is built.",
                    declared.mem,
                    declared.steps,
                    needed.mem,
                    needed.steps
                ));
            }
        }

        Ok(())
    }

    /// Submit a transaction (CBOR bytes)
    pub async fn submit_tx(&self, tx_cbor: &[u8]) -> Result<String> {
        self.preflight(tx_cbor).await?;
        let tx_hash = self.post_cbor("/tx/submit", tx_cbor).await?;
        // Response is the tx hash as a JSON string
        Ok(tx_hash.trim_matches('"').to_string())
    }

    /// Submit a transaction and optionally wait for on-chain confirmation
    pub async fn submit_and_confirm(&self, tx_cbor: &[u8], no_wait: bool) -> Result<String> {
        let tx_hash = self.submit_tx(tx_cbor).await?;
        println!("  TX Hash: {}", tx_hash);
        if !no_wait {
            println!("  Waiting for confirmation...");
            self.wait_for_tx(&tx_hash, 120).await?;
            println!("  Confirmed");
        }
        Ok(tx_hash)
    }

    /// Get transaction details
    pub async fn get_tx(&self, tx_hash: &str) -> Result<TxInfo> {
        self.get(&format!("/txs/{}", tx_hash)).await
    }

    /// Get transactions for an address (single page, returns tx hashes in order)
    pub async fn get_address_transactions(
        &self,
        address: &str,
        count: u32,
    ) -> Result<Vec<AddressTx>> {
        let endpoint = format!(
            "/addresses/{}/transactions?count={}&order=desc",
            address, count
        );
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

    /// Get ALL transactions for an address (paginated, ascending order).
    pub async fn get_all_address_transactions(&self, address: &str) -> Result<Vec<AddressTx>> {
        let mut all_txs = Vec::new();
        let mut page = 1u32;
        loop {
            let endpoint = format!(
                "/addresses/{}/transactions?count=100&page={}&order=asc",
                address, page
            );
            let batch: Vec<AddressTx> = match self.get(&endpoint).await {
                Ok(txs) => txs,
                Err(e) => {
                    if e.to_string().contains("404") {
                        break;
                    }
                    return Err(e);
                }
            };
            let done = batch.len() < 100;
            all_txs.extend(batch);
            if done {
                break;
            }
            page += 1;
        }
        Ok(all_txs)
    }

    /// Get transaction UTXOs (inputs and outputs)
    pub async fn get_tx_utxos(&self, tx_hash: &str) -> Result<TxUtxos> {
        self.get(&format!("/txs/{}/utxos", tx_hash)).await
    }

    /// Get transaction redeemers
    pub async fn get_tx_redeemers(&self, tx_hash: &str) -> Result<Vec<TxRedeemer>> {
        let endpoint = format!("/txs/{}/redeemers", tx_hash);
        match self.get(&endpoint).await {
            Ok(r) => Ok(r),
            Err(e) => {
                if e.to_string().contains("404") {
                    return Ok(vec![]);
                }
                Err(e)
            }
        }
    }

    /// Get script datum by datum hash
    pub async fn get_script_datum(&self, datum_hash: &str) -> Result<ScriptDatum> {
        self.get(&format!("/scripts/datum/{}", datum_hash)).await
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

/// Transaction redeemer
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxRedeemer {
    pub tx_index: u32,
    pub purpose: String,
    pub script_hash: String,
    pub redeemer_data_hash: String,
    pub unit_mem: String,
    pub unit_steps: String,
    pub fee: String,
}

/// Script datum (fetched by datum hash)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptDatum {
    pub json_value: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ogmios_v6_budgets() {
        let json = serde_json::json!({
            "result": [
                {"validator": {"purpose": "spend", "index": 1},
                 "budget": {"memory": 1234, "cpu": 5678}}
            ]
        });
        let units = parse_evaluated_ex_units(&json).unwrap();
        let spend = units.get("spend:1").expect("spend:1 present");
        assert_eq!((spend.mem, spend.steps), (1234, 5678));
    }

    #[test]
    fn parses_ogmios_v5_budgets() {
        // v5 nests under EvaluationResult and names the step field differently.
        let json = serde_json::json!({
            "result": {"EvaluationResult": {"mint:0": {"memory": 11, "steps": 22}}}
        });
        let units = parse_evaluated_ex_units(&json).unwrap();
        let mint = units.get("mint:0").expect("mint:0 present");
        assert_eq!((mint.mem, mint.steps), (11, 22));
    }

    #[test]
    fn script_failure_is_an_error_carrying_the_traces() {
        let json = serde_json::json!({
            "result": {"EvaluationFailure": {"ScriptFailures": {
                "spend:0": [{"validatorFailed": {"traces": ["expect failed"]}}]
            }}}
        });
        let err = parse_evaluated_ex_units(&json).unwrap_err().to_string();
        assert!(err.contains("expect failed"), "traces must survive: {err}");
    }
}
