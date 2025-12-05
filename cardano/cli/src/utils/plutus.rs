//! Plutus/Aiken utilities for working with validators

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

use super::crypto::{script_hash_from_hex, script_address_bech32};

/// Plutus blueprint (plutus.json output from Aiken)
#[derive(Debug, Clone, Deserialize)]
pub struct PlutusBlueprint {
    pub preamble: Preamble,
    pub validators: Vec<ValidatorDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Preamble {
    pub title: String,
    pub version: String,
    #[serde(rename = "plutusVersion")]
    pub plutus_version: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ValidatorDef {
    pub title: String,
    #[serde(rename = "compiledCode")]
    pub compiled_code: String,
    pub hash: String,
    #[serde(default)]
    pub parameters: Vec<ParameterDef>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ParameterDef {
    pub title: String,
    #[allow(dead_code)]
    pub schema: serde_json::Value,
}

impl PlutusBlueprint {
    /// Load blueprint from file
    pub fn from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read plutus.json: {:?}", path))?;
        serde_json::from_str(&content)
            .with_context(|| "Failed to parse plutus.json")
    }

    /// Find a validator by title (e.g., "mailbox.mailbox.spend")
    pub fn find_validator(&self, title: &str) -> Option<&ValidatorDef> {
        self.validators.iter().find(|v| v.title == title)
    }

    /// Get all spend validators
    pub fn spend_validators(&self) -> Vec<&ValidatorDef> {
        self.validators
            .iter()
            .filter(|v| v.title.ends_with(".spend"))
            .collect()
    }

    /// Get all mint validators (minting policies)
    pub fn mint_validators(&self) -> Vec<&ValidatorDef> {
        self.validators
            .iter()
            .filter(|v| v.title.ends_with(".mint"))
            .collect()
    }
}

/// Extracted validator ready for deployment
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedValidator {
    pub name: String,
    pub title: String,
    #[serde(rename = "type")]
    pub script_type: String,
    pub description: String,
    #[serde(rename = "cborHex")]
    pub cbor_hex: String,
    pub hash: String,
    pub address: String,
    /// Whether this validator requires parameter application
    #[serde(default)]
    pub requires_parameters: bool,
    /// Parameter names from the blueprint
    #[serde(default)]
    pub parameter_names: Vec<String>,
}

impl ExtractedValidator {
    /// Create from validator definition
    pub fn from_def(def: &ValidatorDef, network: pallas_addresses::Network) -> Result<Self> {
        let hash = script_hash_from_hex(&def.compiled_code)?;
        let hash_hex = hex::encode(hash);
        let address = script_address_bech32(&hash, network);

        // Extract short name from title (e.g., "mailbox.mailbox.spend" -> "mailbox")
        let name = def.title
            .split('.')
            .next()
            .unwrap_or(&def.title)
            .to_string();

        // Track parameter information
        let requires_parameters = !def.parameters.is_empty();
        let parameter_names = def.parameters.iter().map(|p| p.title.clone()).collect();

        Ok(Self {
            name,
            title: def.title.clone(),
            script_type: "PlutusScriptV3".to_string(),
            description: def.title.clone(),
            cbor_hex: def.compiled_code.clone(),
            hash: hash_hex,
            address,
            requires_parameters,
            parameter_names,
        })
    }

    /// Save to a .plutus file (Cardano CLI format)
    pub fn save_plutus_file(&self, path: &Path) -> Result<()> {
        #[derive(Serialize)]
        struct PlutusFile {
            #[serde(rename = "type")]
            script_type: String,
            description: String,
            #[serde(rename = "cborHex")]
            cbor_hex: String,
        }

        let file = PlutusFile {
            script_type: self.script_type.clone(),
            description: self.description.clone(),
            cbor_hex: self.cbor_hex.clone(),
        };

        let content = serde_json::to_string_pretty(&file)?;
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write {:?}", path))?;
        Ok(())
    }
}

/// Core Hyperlane validators
pub struct HyperlaneValidators {
    pub mailbox: ExtractedValidator,
    pub ism: ExtractedValidator,
    pub registry: ExtractedValidator,
    pub igp: Option<ExtractedValidator>,
    pub validator_announce: Option<ExtractedValidator>,
    pub warp_route: Option<ExtractedValidator>,
    pub vault: Option<ExtractedValidator>,
    pub state_nft: Option<ExtractedValidator>,
}

impl HyperlaneValidators {
    /// Extract all Hyperlane validators from blueprint
    pub fn extract(blueprint: &PlutusBlueprint, network: pallas_addresses::Network) -> Result<Self> {
        let find = |name: &str| -> Result<ExtractedValidator> {
            let title = format!("{}.{}.spend", name, name);
            let def = blueprint
                .find_validator(&title)
                .ok_or_else(|| anyhow!("Validator {} not found", title))?;
            ExtractedValidator::from_def(def, network)
        };

        let find_opt = |name: &str, suffix: &str| -> Option<ExtractedValidator> {
            let title = format!("{}.{}.{}", name, name, suffix);
            blueprint
                .find_validator(&title)
                .and_then(|def| ExtractedValidator::from_def(def, network).ok())
        };

        Ok(Self {
            mailbox: find("mailbox")?,
            ism: find("multisig_ism")?,
            registry: find("registry")?,
            igp: find_opt("igp", "spend"),
            validator_announce: find_opt("validator_announce", "spend"),
            warp_route: find_opt("warp_route", "spend"),
            vault: find_opt("vault", "spend"),
            state_nft: find_opt("state_nft", "mint"),
        })
    }
}

/// Apply parameters to a validator using the aiken CLI
///
/// Find the aiken binary in common locations
fn find_aiken() -> Option<std::path::PathBuf> {
    // Check if aiken is in PATH first
    if let Ok(output) = std::process::Command::new("which").arg("aiken").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }

    // Check common installation paths
    if let Ok(home) = std::env::var("HOME") {
        let aiken_home = std::path::PathBuf::from(&home).join(".aiken/bin/aiken");
        if aiken_home.exists() {
            return Some(aiken_home);
        }

        let cargo_bin = std::path::PathBuf::from(&home).join(".cargo/bin/aiken");
        if cargo_bin.exists() {
            return Some(cargo_bin);
        }
    }

    None
}

/// This shells out to `aiken blueprint apply` to apply CBOR parameters
/// to a parameterized validator in the blueprint.
pub fn apply_validator_param(
    contracts_dir: &Path,
    module: &str,
    validator: &str,
    param_cbor_hex: &str,
) -> Result<AppliedValidator> {
    apply_validator_param_with_purpose(contracts_dir, module, validator, None, param_cbor_hex)
}

/// Apply a parameter to a validator with explicit purpose (mint, spend, else)
pub fn apply_validator_param_with_purpose(
    contracts_dir: &Path,
    module: &str,
    validator: &str,
    purpose: Option<&str>,
    param_cbor_hex: &str,
) -> Result<AppliedValidator> {
    use std::process::Command;

    // Find aiken binary
    let aiken_path = find_aiken()
        .ok_or_else(|| anyhow!("aiken not found. Please install aiken: https://aiken-lang.org/installation-instructions"))?;

    // Create a temporary output file - use just the filename since we'll cd into contracts_dir
    let temp_filename = format!("{}_{}_applied.json", module, validator);
    let temp_file = contracts_dir.join(&temp_filename);

    // Run aiken blueprint apply from contracts directory
    let output = Command::new(&aiken_path)
        .current_dir(contracts_dir)
        .args([
            "blueprint",
            "apply",
            param_cbor_hex,
            "--module",
            module,
            "--validator",
            validator,
            "--out",
            &temp_filename, // Use just filename since we're in contracts_dir
        ])
        .output()
        .with_context(|| format!("Failed to run aiken blueprint apply ({})", aiken_path.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let combined = if stderr.is_empty() { stdout } else { stderr };
        return Err(anyhow!("aiken blueprint apply failed: {}", combined));
    }

    // Read the applied blueprint
    let content = std::fs::read_to_string(&temp_file)
        .with_context(|| format!("Failed to read applied blueprint: {:?}", temp_file))?;

    let blueprint: serde_json::Value = serde_json::from_str(&content)?;

    // Find the applied validator
    let validators = blueprint["validators"]
        .as_array()
        .ok_or_else(|| anyhow!("No validators in applied blueprint"))?;

    // Build title pattern - format is "module.validator.purpose" (e.g., "state_nft.state_nft.mint")
    let title_pattern = match purpose {
        Some(p) => format!("{}.{}.{}", module, validator, p),
        None => format!("{}.{}", module, validator),
    };

    // Try exact match first, then try with common purposes
    let validator_def = validators
        .iter()
        .find(|v| v["title"].as_str() == Some(&title_pattern))
        .or_else(|| {
            // If no explicit purpose given, try common ones
            if purpose.is_none() {
                for p in ["mint", "spend"] {
                    let pattern = format!("{}.{}.{}", module, validator, p);
                    if let Some(v) = validators.iter().find(|v| v["title"].as_str() == Some(&pattern)) {
                        return Some(v);
                    }
                }
            }
            None
        })
        .ok_or_else(|| anyhow!("Applied validator not found: {}", title_pattern))?;

    let compiled_code = validator_def["compiledCode"]
        .as_str()
        .ok_or_else(|| anyhow!("No compiledCode in applied validator"))?
        .to_string();

    // Compute the script hash (policy ID)
    let script_hash = super::crypto::script_hash_from_hex(&compiled_code)?;
    let policy_id = hex::encode(script_hash);

    // Clean up temp file
    let _ = std::fs::remove_file(&temp_file);

    Ok(AppliedValidator {
        compiled_code,
        policy_id,
    })
}

/// Apply multiple parameters to a validator sequentially
/// Each parameter is applied in order, using the output of the previous apply as input
pub fn apply_validator_params(
    contracts_dir: &Path,
    module: &str,
    validator: &str,
    params_cbor_hex: &[&str],
) -> Result<AppliedValidator> {
    use std::process::Command;

    if params_cbor_hex.is_empty() {
        return Err(anyhow!("At least one parameter is required"));
    }

    let aiken_path = find_aiken()
        .ok_or_else(|| anyhow!("aiken not found. Please install aiken: https://aiken-lang.org/installation-instructions"))?;

    let mut current_blueprint: Option<String> = None;
    let temp_base = format!("{}_{}_multi", module, validator);

    for (i, param) in params_cbor_hex.iter().enumerate() {
        let out_filename = format!("{}_{}.json", temp_base, i);
        let _out_file = contracts_dir.join(&out_filename);

        let mut args = vec![
            "blueprint".to_string(),
            "apply".to_string(),
            param.to_string(),
            "--module".to_string(),
            module.to_string(),
            "--validator".to_string(),
            validator.to_string(),
            "--out".to_string(),
            out_filename.clone(),
        ];

        // Use previous output as input for subsequent applies
        if let Some(ref in_file) = current_blueprint {
            args.push("--in".to_string());
            args.push(in_file.clone());
        }

        let output = Command::new(&aiken_path)
            .current_dir(contracts_dir)
            .args(&args)
            .output()
            .with_context(|| format!("Failed to run aiken blueprint apply ({})", aiken_path.display()))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let combined = if stderr.is_empty() { stdout } else { stderr };
            // Clean up temp files
            for j in 0..=i {
                let _ = std::fs::remove_file(contracts_dir.join(format!("{}_{}.json", temp_base, j)));
            }
            return Err(anyhow!("aiken blueprint apply failed (param {}): {}", i + 1, combined));
        }

        // Clean up previous temp file
        if let Some(ref prev) = current_blueprint {
            let _ = std::fs::remove_file(contracts_dir.join(prev));
        }

        current_blueprint = Some(out_filename);
    }

    // Read the final applied blueprint
    let final_file = contracts_dir.join(current_blueprint.as_ref().unwrap());
    let content = std::fs::read_to_string(&final_file)
        .with_context(|| format!("Failed to read applied blueprint: {:?}", final_file))?;

    let blueprint: serde_json::Value = serde_json::from_str(&content)?;

    // Find the applied validator
    let validators = blueprint["validators"]
        .as_array()
        .ok_or_else(|| anyhow!("No validators in applied blueprint"))?;

    let title_pattern = format!("{}.{}", module, validator);

    let validator_def = validators
        .iter()
        .find(|v| v["title"].as_str() == Some(&title_pattern))
        .or_else(|| {
            for p in ["mint", "spend"] {
                let pattern = format!("{}.{}.{}", module, validator, p);
                if let Some(v) = validators.iter().find(|v| v["title"].as_str() == Some(&pattern)) {
                    return Some(v);
                }
            }
            None
        })
        .ok_or_else(|| anyhow!("Applied validator not found: {}", title_pattern))?;

    let compiled_code = validator_def["compiledCode"]
        .as_str()
        .ok_or_else(|| anyhow!("No compiledCode in applied validator"))?
        .to_string();

    let script_hash = super::crypto::script_hash_from_hex(&compiled_code)?;
    let policy_id = hex::encode(script_hash);

    // Clean up final temp file
    let _ = std::fs::remove_file(&final_file);

    Ok(AppliedValidator {
        compiled_code,
        policy_id,
    })
}

/// Result of applying parameters to a validator
#[derive(Debug, Clone)]
pub struct AppliedValidator {
    pub compiled_code: String,
    pub policy_id: String,
}

impl AppliedValidator {
    /// Create a Cardano CLI compatible plutus script file
    pub fn to_plutus_json(&self, description: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "PlutusScriptV3",
            "description": description,
            "cborHex": self.compiled_code
        })
    }

    /// Save to a .plutus file
    pub fn save_plutus_file(&self, path: &Path, description: &str) -> Result<()> {
        let json = self.to_plutus_json(description);
        let content = serde_json::to_string_pretty(&json)?;
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write {:?}", path))?;
        Ok(())
    }
}

/// Convert a script hash (hex) to a bech32 address
pub fn script_hash_to_address(hash_hex: &str, network: pallas_addresses::Network) -> Result<String> {
    let hash_bytes = hex::decode(hash_hex)
        .map_err(|e| anyhow!("Invalid script hash hex: {}", e))?;

    if hash_bytes.len() != 28 {
        return Err(anyhow!("Script hash must be 28 bytes, got {}", hash_bytes.len()));
    }

    let mut hash_array = [0u8; 28];
    hash_array.copy_from_slice(&hash_bytes);

    Ok(script_address_bech32(&hash_array, network))
}

/// Encode a script hash (28 bytes) as CBOR for validator parameters
pub fn encode_script_hash_param(script_hash_hex: &str) -> Result<Vec<u8>> {
    let hash_bytes = hex::decode(script_hash_hex)
        .with_context(|| "Invalid script hash hex")?;

    if hash_bytes.len() != 28 {
        return Err(anyhow!("Script hash must be 28 bytes, got {}", hash_bytes.len()));
    }

    // CBOR encoding for ByteArray (28 bytes):
    // 581c = 28-byte bytestring prefix
    let mut cbor = vec![0x58, 0x1c];
    cbor.extend_from_slice(&hash_bytes);

    Ok(cbor)
}

/// Encode an output reference as CBOR for state_nft parameter
pub fn encode_output_reference(tx_hash: &str, output_index: u32) -> Result<Vec<u8>> {
    let tx_hash_bytes = hex::decode(tx_hash)
        .with_context(|| "Invalid tx hash hex")?;

    if tx_hash_bytes.len() != 32 {
        return Err(anyhow!("TX hash must be 32 bytes"));
    }

    // CBOR encoding for Aiken OutputReference:
    // Constr 0 [ByteArray(tx_hash), Int(output_index)]
    //
    // d8799f = constructor 0 (tag 121) + indefinite array start
    // 5820 = 32-byte bytestring prefix
    // tx_hash = 32 bytes
    // output_index encoded as appropriate int
    // ff = break (end array)

    let mut cbor = vec![0xd8, 0x79, 0x9f, 0x58, 0x20];
    cbor.extend_from_slice(&tx_hash_bytes);

    // Encode output index
    if output_index <= 23 {
        cbor.push(output_index as u8);
    } else if output_index <= 255 {
        cbor.push(0x18);
        cbor.push(output_index as u8);
    } else {
        cbor.push(0x19);
        cbor.extend_from_slice(&(output_index as u16).to_be_bytes());
    }

    cbor.push(0xff); // break

    Ok(cbor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_output_reference() {
        let tx_hash = "a".repeat(64); // 32 bytes of 0xaa
        let result = encode_output_reference(&tx_hash, 0).unwrap();

        assert!(result.starts_with(&[0xd8, 0x79, 0x9f, 0x58, 0x20]));
        assert!(result.ends_with(&[0xff]));
    }

    #[test]
    fn test_encode_output_reference_large_index() {
        let tx_hash = "a".repeat(64);
        let result = encode_output_reference(&tx_hash, 100).unwrap();

        // Should have 0x18 prefix for index > 23
        assert!(result.contains(&0x18));
    }
}
