//! Compares the chain addresses an agent will use with a known-good list.

use std::{collections::HashMap, fs, path::Path, str::FromStr};

use eyre::{bail, eyre, Context, Result};
use serde::Deserialize;
use tracing::warn;

use hyperlane_core::{H160, H256};

use super::{base::Settings, chains::CoreContractAddresses};

const FAILED: &str = "registry address check failed";

/// The known list: one route between a Midnight chain and a remote chain, with
/// every deployment of it that is still trusted. Only the fields the agents
/// compare are read.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownAddresses {
    pub midnight: KnownMidnightChain,
    pub remote: KnownRemoteChain,
    pub deployments: Vec<KnownDeployment>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownMidnightChain {
    pub chain_name: String,
    pub domain: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownRemoteChain {
    pub chain_name: String,
    pub domain: u32,
}

/// A null address is one the list does not know yet and is skipped.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownDeployment {
    pub name: String,
    pub midnight: KnownMidnightAddresses,
    pub remote: HashMap<String, Option<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownMidnightAddresses {
    pub night: Option<String>,
    pub igp: Option<String>,
    pub validator_announce: Option<String>,
}

/// One address from the agent's chain config, keyed the way the known list
/// names it (`midnight.night`, `remote.mailbox`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedAddress {
    pub key: &'static str,
    pub value: H256,
}

impl Settings {
    /// Checks the chain config against the list named by `knownAddresses`.
    /// Unset means no check, with a warning.
    pub fn check_known_addresses(&self) -> Result<()> {
        let Some(path) = &self.known_addresses else {
            warn!(
                "Chain addresses were not checked against a known list (knownAddresses is unset)"
            );
            return Ok(());
        };
        let known = read_known_addresses(path)?;
        let loaded = self.loaded_addresses(&known)?;
        compare_known_addresses(&loaded, &known)
    }

    /// The addresses of the two chains the known list names, for the chains
    /// this agent loaded. A validator loads only its origin chain, so a chain
    /// missing from the config is skipped rather than failed.
    fn loaded_addresses(&self, known: &KnownAddresses) -> Result<Vec<LoadedAddress>> {
        let mut loaded = Vec::new();
        let sides = [
            (
                "midnight",
                &known.midnight.chain_name,
                known.midnight.domain,
            ),
            ("remote", &known.remote.chain_name, known.remote.domain),
        ];
        for (side, chain_name, known_domain) in sides {
            let Some(domain) = self.domains.get(chain_name) else {
                continue;
            };
            let conf = self.chain_setup(domain)?;
            if conf.domain.id() != known_domain {
                bail!(
                    "{FAILED}: {side}.domain is {}, the known list has {known_domain}",
                    conf.domain.id()
                );
            }
            push_loaded(&mut loaded, side, &conf.addresses);
        }
        if loaded.is_empty() {
            bail!(
                "{FAILED}: neither \"{}\" nor \"{}\" from the known list is in the agent's chain config",
                known.midnight.chain_name,
                known.remote.chain_name
            );
        }
        Ok(loaded)
    }
}

// The night contract is the Midnight mailbox and its merkle tree hook.
fn push_loaded(loaded: &mut Vec<LoadedAddress>, side: &str, addresses: &CoreContractAddresses) {
    let fields: &[(&'static str, H256)] = if side == "midnight" {
        &[
            ("midnight.night", addresses.mailbox),
            ("midnight.night", addresses.merkle_tree_hook),
            ("midnight.igp", addresses.interchain_gas_paymaster),
            ("midnight.validatorAnnounce", addresses.validator_announce),
        ]
    } else {
        &[
            ("remote.mailbox", addresses.mailbox),
            ("remote.merkleTreeHook", addresses.merkle_tree_hook),
            (
                "remote.interchainGasPaymaster",
                addresses.interchain_gas_paymaster,
            ),
        ]
    };
    loaded.extend(
        fields
            .iter()
            .map(|(key, value)| LoadedAddress { key, value: *value }),
    );
}

pub fn read_known_addresses(path: &Path) -> Result<KnownAddresses> {
    let text = fs::read_to_string(path)
        .with_context(|| format!("reading the known address list at {}", path.display()))?;
    serde_json::from_str(&text)
        .with_context(|| format!("parsing the known address list at {}", path.display()))
}

fn known_value<'a>(deployment: &'a KnownDeployment, key: &str) -> Option<&'a str> {
    let value = match key {
        "midnight.night" => &deployment.midnight.night,
        "midnight.igp" => &deployment.midnight.igp,
        "midnight.validatorAnnounce" => &deployment.midnight.validator_announce,
        _ => key
            .strip_prefix("remote.")
            .and_then(|field| deployment.remote.get(field))?,
    };
    value.as_deref()
}

/// Known values are hex with or without `0x`: 20 bytes for an EVM address,
/// 32 for a Midnight contract. Either is compared as the left-padded H256 the
/// config parser produces.
fn known_to_h256(value: &str) -> Result<H256> {
    let hex = value.strip_prefix("0x").unwrap_or(value);
    let prefixed = format!("0x{hex}");
    let parsed = match hex.len() {
        40 => H160::from_str(&prefixed).map(Into::into),
        64 => H256::from_str(&prefixed),
        n => bail!("{value} is {n} hex characters, expected 40 or 64"),
    };
    parsed.map_err(|_| eyre!("{value} is not hex"))
}

/// Prints the loaded address at the width the known list uses for it.
fn display_loaded(value: H256, known: &str) -> String {
    if known.strip_prefix("0x").unwrap_or(known).len() == 40 {
        format!("{:?}", H160::from_slice(&value.as_bytes()[12..]))
    } else {
        format!("{value:?}")
    }
}

/// A deployment entry matches when every address it knows that was also
/// loaded is equal. On failure the error names the first changed address of
/// the entry with the most equal addresses.
pub fn compare_known_addresses(loaded: &[LoadedAddress], known: &KnownAddresses) -> Result<()> {
    struct Candidate<'a> {
        deployment: &'a KnownDeployment,
        matched: usize,
        mismatch: (&'a LoadedAddress, &'a str),
    }
    let mut closest: Option<Candidate> = None;

    for deployment in &known.deployments {
        let mut matched: usize = 0;
        let mut mismatch = None;
        for entry in loaded {
            let Some(expected) = known_value(deployment, entry.key) else {
                continue;
            };
            let expected_h256 = known_to_h256(expected).with_context(|| {
                format!(
                    "known list entry {:?} has an invalid {}",
                    deployment.name, entry.key
                )
            })?;
            if expected_h256 == entry.value {
                matched = matched.saturating_add(1);
            } else if mismatch.is_none() {
                mismatch = Some((entry, expected));
            }
        }
        let Some(mismatch) = mismatch else {
            return Ok(());
        };
        if closest.as_ref().is_none_or(|c| matched > c.matched) {
            closest = Some(Candidate {
                deployment,
                matched,
                mismatch,
            });
        }
    }

    let Candidate {
        deployment,
        mismatch: (entry, expected),
        ..
    } = closest.ok_or_else(|| eyre!("{FAILED}: the known list has no deployments"))?;
    bail!(
        "{FAILED}: {} is {}, the known list has {expected} (deployment \"{}\")",
        entry.key,
        display_loaded(entry.value, expected),
        deployment.name
    )
}

#[cfg(test)]
mod test {
    use std::io::Write;

    use hyperlane_core::config::{ConfigPath, FromRawConf};
    use serde_json::{json, Value};

    use super::*;
    use crate::settings::parser::RawAgentConf;

    const NIGHT: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const OLD_NIGHT: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const IGP: &str = "2222222222222222222222222222222222222222222222222222222222222222";
    const VA: &str = "3333333333333333333333333333333333333333333333333333333333333333";

    fn known_list() -> Value {
        json!({
            "route": "NIGHT/midnight1-test4",
            "midnight": { "chainName": "midnight1", "network": "devnet", "domain": 12340 },
            "remote": { "chainName": "test4", "domain": 31337 },
            "deployments": [
                {
                    "name": "July 2026",
                    "deployed": "2026-07-24",
                    "midnight": { "night": OLD_NIGHT, "igp": IGP, "validatorAnnounce": VA },
                    "remote": {
                        "tokenRouter": "0x00000000000000000000000000000000000000a1",
                        "tokenIsm": "0x00000000000000000000000000000000000000e1",
                        "mailbox": "0x00000000000000000000000000000000000000b1",
                        "merkleTreeHook": "0x00000000000000000000000000000000000000c1",
                        "interchainGasPaymaster": "0x00000000000000000000000000000000000000d1"
                    }
                },
                {
                    "name": "August 2026",
                    "deployed": "2026-08-20",
                    "midnight": { "night": NIGHT, "igp": IGP, "validatorAnnounce": VA },
                    "remote": {
                        "tokenRouter": "0x00000000000000000000000000000000000000a2",
                        "tokenIsm": "0x00000000000000000000000000000000000000e2",
                        "mailbox": "0x00000000000000000000000000000000000000b2",
                        "merkleTreeHook": "0x00000000000000000000000000000000000000c2",
                        "interchainGasPaymaster": "0x00000000000000000000000000000000000000d2"
                    }
                }
            ]
        })
    }

    fn known() -> KnownAddresses {
        serde_json::from_value(known_list()).unwrap()
    }

    fn chain_config(
        name: &str,
        domain_id: u32,
        mailbox: &str,
        igp: &str,
        validator_announce: &str,
        merkle_tree_hook: &str,
    ) -> Value {
        json!({
            "name": name,
            "domainid": domain_id,
            "chainid": domain_id,
            "protocol": "ethereum",
            "rpcurls": [{ "http": "http://localhost:8545" }],
            "mailbox": mailbox,
            "interchaingaspaymaster": igp,
            "validatorannounce": validator_announce,
            "merkletreehook": merkle_tree_hook,
        })
    }

    // The agent's chain config as the August deployment: Midnight addresses
    // 32 bytes, EVM addresses checksummed.
    fn august_chains() -> Value {
        json!({
            "midnight1": chain_config(
                "midnight1", 12340,
                &format!("0x{NIGHT}"), &format!("0x{IGP}"), &format!("0x{VA}"), &format!("0x{NIGHT}"),
            ),
            "test4": chain_config(
                "test4", 31337,
                "0x00000000000000000000000000000000000000B2",
                "0x00000000000000000000000000000000000000D2",
                "0x00000000000000000000000000000000000000F2",
                "0x00000000000000000000000000000000000000C2",
            ),
        })
    }

    fn parse_settings(chains: Value, known_addresses: Option<&str>) -> Settings {
        let mut raw = json!({ "chains": chains });
        if let Some(path) = known_addresses {
            raw["knownaddresses"] = json!(path);
        }
        let raw: RawAgentConf = serde_json::from_value(raw).unwrap();
        Settings::from_config_filtered(raw, &ConfigPath::default(), None, "relayer").unwrap()
    }

    fn known_file(contents: &Value) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(contents.to_string().as_bytes()).unwrap();
        file
    }

    fn check(chains: Value, list: &Value) -> Result<()> {
        let file = known_file(list);
        parse_settings(chains, file.path().to_str()).check_known_addresses()
    }

    #[test]
    fn parses_the_setting_as_a_path() {
        let settings = parse_settings(august_chains(), Some("/etc/hyperlane/known.json"));
        assert_eq!(
            settings.known_addresses.as_deref(),
            Some(Path::new("/etc/hyperlane/known.json"))
        );
        assert_eq!(parse_settings(august_chains(), None).known_addresses, None);
    }

    #[test]
    fn unset_setting_skips_the_check() {
        parse_settings(august_chains(), None)
            .check_known_addresses()
            .unwrap();
    }

    #[test]
    fn passes_when_both_chains_match_one_deployment() {
        check(august_chains(), &known_list()).unwrap();
    }

    #[test]
    fn passes_for_a_validator_that_loaded_one_chain() {
        let mut chains = august_chains();
        chains.as_object_mut().unwrap().remove("test4");
        check(chains, &known_list()).unwrap();
    }

    #[test]
    fn fails_when_neither_chain_is_loaded() {
        let chains = json!({
            "other": chain_config(
                "other", 990099,
                "0x00000000000000000000000000000000000000B2",
                "0x00000000000000000000000000000000000000D2",
                "0x00000000000000000000000000000000000000F2",
                "0x00000000000000000000000000000000000000C2",
            ),
        });
        let error = check(chains, &known_list()).unwrap_err().to_string();
        assert_eq!(
            error,
            "registry address check failed: neither \"midnight1\" nor \"test4\" from the known list is in the agent's chain config"
        );
    }

    #[test]
    fn fails_on_a_domain_that_differs_from_the_known_list() {
        let mut chains = august_chains();
        chains["midnight1"]["domainid"] = json!(4321);
        let error = check(chains, &known_list()).unwrap_err().to_string();
        assert_eq!(
            error,
            "registry address check failed: midnight.domain is 4321, the known list has 12340"
        );
    }

    #[test]
    fn names_the_first_tampered_address_of_the_closest_entry() {
        let mut chains = august_chains();
        chains["test4"]["merkletreehook"] = json!(format!("0x{}", "ff".repeat(20)));
        let error = check(chains, &known_list()).unwrap_err().to_string();
        assert_eq!(
            error,
            format!(
                "registry address check failed: remote.merkleTreeHook is 0x{}, the known list has 0x00000000000000000000000000000000000000c2 (deployment \"August 2026\")",
                "ff".repeat(20)
            )
        );
    }

    #[test]
    fn catches_a_midnight_merkle_tree_hook_that_is_not_the_night_contract() {
        let mut chains = august_chains();
        chains["midnight1"]["merkletreehook"] = json!(format!("0x{OLD_NIGHT}"));
        let error = check(chains, &known_list()).unwrap_err().to_string();
        assert_eq!(
            error,
            format!(
                "registry address check failed: midnight.night is 0x{OLD_NIGHT}, the known list has {NIGHT} (deployment \"August 2026\")"
            )
        );
    }

    #[test]
    fn reports_against_the_entry_with_the_most_matching_fields() {
        // July wins on the midnight fields, August on the three remote ones.
        let mut chains = august_chains();
        chains["midnight1"]["mailbox"] = json!(format!("0x{OLD_NIGHT}"));
        chains["midnight1"]["merkletreehook"] = json!(format!("0x{OLD_NIGHT}"));
        let error = check(chains, &known_list()).unwrap_err().to_string();
        assert_eq!(
            error,
            format!(
                "registry address check failed: midnight.night is 0x{OLD_NIGHT}, the known list has {NIGHT} (deployment \"August 2026\")"
            )
        );
    }

    #[test]
    fn compares_known_values_with_or_without_prefix() {
        let mut list = known_list();
        list["deployments"][1]["midnight"]["night"] = json!(format!("0x{}", NIGHT.to_uppercase()));
        list["deployments"][1]["remote"]["mailbox"] =
            json!("00000000000000000000000000000000000000b2");
        check(august_chains(), &list).unwrap();
    }

    #[test]
    fn skips_null_fields_recorded_before_the_remote_side_exists() {
        let mut list = known_list();
        list["deployments"] = json!([{
            "name": "September 2026",
            "deployed": "2026-09-01",
            "midnight": { "night": NIGHT, "igp": IGP, "validatorAnnounce": VA },
            "remote": {
                "tokenRouter": null,
                "tokenIsm": null,
                "mailbox": null,
                "merkleTreeHook": null,
                "interchainGasPaymaster": null
            }
        }]);
        check(august_chains(), &list).unwrap();
    }

    #[test]
    fn rejects_a_malformed_known_value() {
        let mut list = known_list();
        list["deployments"][1]["remote"]["mailbox"] = json!("0x1234");
        let error = format!("{:#}", check(august_chains(), &list).unwrap_err());
        assert!(
            error.contains("expected 40 or 64"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn ignores_known_fields_the_config_does_not_carry() {
        let loaded = [LoadedAddress {
            key: "midnight.night",
            value: known_to_h256(NIGHT).unwrap(),
        }];
        compare_known_addresses(&loaded, &known()).unwrap();
    }
}
