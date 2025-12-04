# Hyperlane Cardano: Reference Script UTXO Handling

## Problem Statement

When spending a script UTXO on Cardano, the transaction needs access to the validator code. There are two mechanisms:

1. **Witness script** - Include the full script in the transaction (expensive, bloats tx size)
2. **Reference script** - Point to a UTXO that contains the script (cheaper, preferred post-Vasil)

The Hyperlane relayer needs to know: **"Where is the UTXO containing the recipient's validator code?"**

This document describes how to handle reference script discovery in the Hyperlane Cardano integration.

---

## Recommended Solution: Explicit Registration

Add a `reference_script_locator` field to the recipient registration:

```aiken
type RecipientRegistration {
  script_hash: ScriptHash,
  
  // Where to find the state UTXO (contains datum)
  state_locator: UtxoLocator,
  
  // Where to find the reference script UTXO
  // If None, assume script is embedded in state_locator UTXO
  reference_script_locator: Option<UtxoLocator>,
  
  additional_inputs: List<AdditionalInput>,
  recipient_type: RecipientType,
  custom_ism: Option<ScriptHash>,
}
```

---

## Architecture

### Two-UTXO Pattern (Recommended)

Separate the state UTXO from the reference script UTXO:

```
┌─────────────────────────────────────────┐
│ Reference Script UTXO (never spent)     │
│                                         │
│ Address: deployer address or "holder"   │
│ Value: ~20-30 ADA + NFT(policy, "ref")  │
│ Datum: None                             │
│ Reference Script: <validator code>   ◀──│── Script lives here
└─────────────────────────────────────────┘

┌─────────────────────────────────────────┐
│ Recipient State UTXO (spent on handle)  │
│                                         │
│ Address: script address                 │
│ Value: ~2 ADA + NFT(policy, "state")    │
│ Datum: { contract state... }            │
│ Reference Script: None                  │
└─────────────────────────────────────────┘
```

**Advantages:**
- State UTXO is smaller → less locked ADA
- Reference script UTXO is immutable → stable, never changes
- Common pattern in Cardano ecosystem
- Clear separation of concerns

**Disadvantages:**
- Two UTXOs to manage during deployment
- Need to track both locators

### Alternative: Script in State UTXO

For simpler cases, embed the script in the state UTXO:

```
┌─────────────────────────────────────────┐
│ Recipient State UTXO                    │
│                                         │
│ Address: script address                 │
│ Value: ~25 ADA + NFT(policy, "state")   │
│ Datum: { contract state... }            │
│ Reference Script: <validator code>   ◀──│── Script embedded
└─────────────────────────────────────────┘
```

**Advantages:**
- Single UTXO, single locator
- Simpler deployment

**Disadvantages:**
- Larger UTXO → more ADA locked for min UTXO requirement
- Script is "re-attached" to output every time UTXO is spent/recreated
- Slightly higher tx costs when spending

---

## Updated Data Types

### UtxoLocator (unchanged)

```aiken
type UtxoLocator {
  policy_id: PolicyId,
  asset_name: AssetName,
}
```

### RecipientRegistration (updated)

```aiken
type RecipientRegistration {
  // Script hash of the recipient validator
  script_hash: ScriptHash,
  
  // NFT locator to find the state UTXO
  state_locator: UtxoLocator,
  
  // NFT locator to find the reference script UTXO
  // None = script is embedded in state UTXO
  reference_script_locator: Option<UtxoLocator>,
  
  // Additional UTXOs needed (vaults, pools, etc.)
  additional_inputs: List<AdditionalInput>,
  
  // How the relayer should construct outputs
  recipient_type: RecipientType,
  
  // Override default ISM (None = use mailbox default)
  custom_ism: Option<ScriptHash>,
}
```

---

## Relayer UTXO Discovery Flow

```
Message arrives: { recipient: 0x000...abc123, body: ... }
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 1. Query Registry for script_hash abc123        │
│                                                 │
│    Result:                                      │
│    - state_locator: NFT(policy_A, "state")      │
│    - reference_script_locator:                  │
│        Some(NFT(policy_A, "ref"))               │
│    - additional_inputs: [vault: NFT(...)]       │
│    - recipient_type: TokenReceiver              │
└─────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 2. Discover all UTXOs by NFT                    │
│                                                 │
│    State UTXO:                                  │
│    └─ Query: UTXO containing NFT(policy_A,      │
│              "state")                           │
│    └─ Found: tx_abc#0 at script address         │
│                                                 │
│    Reference Script UTXO:                       │
│    └─ Query: UTXO containing NFT(policy_A,      │
│              "ref")                             │
│    └─ Found: tx_def#0 at deployer address       │
│                                                 │
│    Additional (vault):                          │
│    └─ Query: UTXO containing vault NFT          │
│    └─ Found: tx_ghi#1 at vault script           │
└─────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 3. Build Transaction                            │
│                                                 │
│    Reference Inputs (read-only):                │
│    ├─ tx_def#0 (reference script for recipient) │
│    ├─ ISM UTXO (for verification)               │
│    └─ Registry UTXO (optional, for validation)  │
│                                                 │
│    Script Inputs (spent):                       │
│    ├─ Mailbox UTXO + Process redeemer           │
│    ├─ tx_abc#0 (state) + HandleMessage redeemer │
│    └─ tx_ghi#1 (vault) + Release redeemer       │
│                                                 │
│    Outputs:                                     │
│    ├─ Mailbox continuation                      │
│    ├─ Recipient state continuation              │
│    ├─ Vault continuation (minus tokens)         │
│    └─ User receives tokens                      │
└─────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 4. Sign and Submit                              │
│                                                 │
│    On UTXO contention failure:                  │
│    └─ Re-run from step 2 with fresh queries     │
└─────────────────────────────────────────────────┘
```

---

## Relayer Implementation (Rust)

### UTXO Discovery

```rust
// relayer/src/cardano/utxo_discovery.rs

use cardano_serialization_lib as csl;

pub struct DiscoveredUtxos {
    pub mailbox: Utxo,
    pub mailbox_ref_script: Utxo,
    pub ism: Utxo,
    pub ism_ref_script: Utxo,
    pub recipient_state: Utxo,
    pub recipient_ref_script: Utxo,  // NEW
    pub additional: Vec<(AdditionalInput, Utxo)>,
}

impl CardanoAdapter {
    pub async fn discover_utxos(
        &self,
        registration: &RecipientRegistration,
    ) -> Result<DiscoveredUtxos> {
        // Mailbox state and reference script (known locations)
        let mailbox = self.find_utxo_by_nft(&self.config.mailbox_state_nft).await?;
        let mailbox_ref_script = self.find_utxo_by_nft(&self.config.mailbox_ref_nft).await?;
        
        // ISM state and reference script
        let ism_hash = registration.custom_ism
            .unwrap_or(self.config.default_ism);
        let ism_registration = self.lookup_ism_registration(&ism_hash).await?;
        let ism = self.find_utxo_by_nft(&ism_registration.state_locator).await?;
        let ism_ref_script = self.find_utxo_by_nft(
            &ism_registration.reference_script_locator
        ).await?;
        
        // Recipient state UTXO
        let recipient_state = self.find_utxo_by_nft(
            &registration.state_locator
        ).await?;
        
        // Recipient reference script UTXO
        let recipient_ref_script = match &registration.reference_script_locator {
            Some(locator) => self.find_utxo_by_nft(locator).await?,
            None => {
                // Script is embedded in state UTXO
                // Verify it actually has a reference script
                if recipient_state.reference_script.is_none() {
                    return Err(Error::MissingReferenceScript);
                }
                recipient_state.clone()
            }
        };
        
        // Additional inputs
        let mut additional = Vec::new();
        for input in &registration.additional_inputs {
            let utxo = self.find_utxo_by_nft(&input.locator).await?;
            additional.push((input.clone(), utxo));
        }
        
        Ok(DiscoveredUtxos {
            mailbox,
            mailbox_ref_script,
            ism,
            ism_ref_script,
            recipient_state,
            recipient_ref_script,
            additional,
        })
    }
    
    async fn find_utxo_by_nft(&self, locator: &UtxoLocator) -> Result<Utxo> {
        // Query chain for UTXO containing the specified NFT
        let utxos = self.node_client
            .query_utxos_by_asset(&locator.policy_id, &locator.asset_name)
            .await?;
        
        // Should be exactly one (NFT is unique)
        match utxos.len() {
            0 => Err(Error::UtxoNotFound {
                policy: locator.policy_id.clone(),
                name: locator.asset_name.clone(),
            }),
            1 => Ok(utxos.into_iter().next().unwrap()),
            _ => Err(Error::MultipleUtxosFound {
                policy: locator.policy_id.clone(),
                name: locator.asset_name.clone(),
                count: utxos.len(),
            }),
        }
    }
}
```

### Transaction Building

```rust
// relayer/src/cardano/tx_builder.rs

impl CardanoAdapter {
    pub fn build_process_tx(
        &self,
        message: &HyperlaneMessage,
        metadata: &[u8],
        registration: &RecipientRegistration,
        utxos: &DiscoveredUtxos,
    ) -> Result<Transaction> {
        let mut tx_builder = TransactionBuilder::new(&self.protocol_params);
        
        // ========== Reference Inputs (read-only) ==========
        
        // Mailbox reference script
        tx_builder.add_reference_input(&utxos.mailbox_ref_script.to_input());
        
        // ISM reference script + state (for signature verification)
        tx_builder.add_reference_input(&utxos.ism_ref_script.to_input());
        tx_builder.add_reference_input(&utxos.ism.to_input());
        
        // Recipient reference script (if separate from state)
        if utxos.recipient_ref_script.output_ref != utxos.recipient_state.output_ref {
            tx_builder.add_reference_input(&utxos.recipient_ref_script.to_input());
        }
        
        // ========== Script Inputs (spent) ==========
        
        // Mailbox - Process redeemer
        let mailbox_redeemer = MailboxRedeemer::Process {
            message: message.clone(),
            metadata: metadata.to_vec(),
            message_id: message.id(),
        };
        tx_builder.add_plutus_script_input(
            &utxos.mailbox.to_input(),
            &encode_redeemer(&mailbox_redeemer)?,
        );
        
        // Recipient state - HandleMessage redeemer
        let recipient_redeemer = HyperlaneRecipientRedeemer::HandleMessage {
            origin: message.origin,
            sender: message.sender.clone(),
            body: message.body.clone(),
        };
        tx_builder.add_plutus_script_input(
            &utxos.recipient_state.to_input(),
            &encode_redeemer(&recipient_redeemer)?,
        );
        
        // Additional inputs (vaults, etc.)
        for (input_spec, utxo) in &utxos.additional {
            if input_spec.must_be_spent {
                let redeemer = self.build_additional_redeemer(input_spec, message)?;
                tx_builder.add_plutus_script_input(
                    &utxo.to_input(),
                    &redeemer,
                );
            } else {
                tx_builder.add_reference_input(&utxo.to_input());
            }
        }
        
        // ========== Outputs ==========
        
        self.build_outputs(&mut tx_builder, message, registration, utxos)?;
        
        // ========== Collateral & Fees ==========
        
        tx_builder.add_collateral(&self.collateral_utxo);
        tx_builder.add_change_address(&self.change_address);
        
        tx_builder.build()
    }
}
```

---

## Deployment Process

### TypeScript Deployment Script

```typescript
// scripts/deploy-recipient.ts

import { Lucid, Script, Data, fromText } from "lucid-cardano";

interface DeploymentResult {
  scriptHash: string;
  stateUtxo: { txHash: string; outputIndex: number };
  refScriptUtxo: { txHash: string; outputIndex: number };
  registration: RecipientRegistration;
}

async function deployRecipient(
  lucid: Lucid,
  validator: Script,
  initialDatum: Data,
): Promise<DeploymentResult> {
  const deployerAddress = await lucid.wallet.address();
  const scriptHash = lucid.utils.validatorToScriptHash(validator);
  const scriptAddress = lucid.utils.validatorToAddress(validator);
  
  // 1. Select a UTXO to consume (for one-shot minting policy)
  const deployUtxo = (await lucid.wallet.getUtxos())[0];
  if (!deployUtxo) throw new Error("No UTXOs available for deployment");
  
  // 2. Create one-shot minting policy
  //    This policy can only mint once - when deployUtxo is consumed
  const oneShotPolicy = lucid.utils.nativeScriptFromJson({
    type: "all",
    scripts: [
      {
        type: "sig",
        keyHash: lucid.utils.getAddressDetails(deployerAddress).paymentCredential!.hash,
      },
      // Must consume the specific UTXO
      {
        type: "before",
        slot: (await lucid.currentSlot()) + 1000,
      },
    ],
  });
  const nftPolicyId = lucid.utils.mintingPolicyToId(oneShotPolicy);
  
  // 3. Build transaction that:
  //    - Consumes deploy UTXO (enables one-shot mint)
  //    - Mints state NFT and ref NFT
  //    - Creates state UTXO at script address
  //    - Creates reference script UTXO at deployer address
  
  const stateNftUnit = nftPolicyId + fromText("state");
  const refNftUnit = nftPolicyId + fromText("ref");
  
  const tx = await lucid
    .newTx()
    .collectFrom([deployUtxo])
    // Mint both NFTs
    .mintAssets(
      {
        [stateNftUnit]: 1n,
        [refNftUnit]: 1n,
      },
      Data.void(), // Native script, no redeemer needed
    )
    .attachMintingPolicy(oneShotPolicy)
    // State UTXO at script address
    .payToContract(
      scriptAddress,
      { inline: initialDatum },
      {
        [stateNftUnit]: 1n,
        lovelace: 2_000_000n, // ~2 ADA for min UTXO
      },
    )
    // Reference script UTXO at deployer address
    .payToAddressWithData(
      deployerAddress,
      {
        inline: Data.void(), // No datum needed
        scriptRef: validator, // Attach the script!
      },
      {
        [refNftUnit]: 1n,
        lovelace: 25_000_000n, // ~25 ADA (depends on script size)
      },
    )
    .complete();
  
  const signedTx = await tx.sign().complete();
  const txHash = await signedTx.submit();
  
  console.log(`Deployment tx submitted: ${txHash}`);
  await lucid.awaitTx(txHash);
  console.log(`Deployment confirmed!`);
  
  // 4. Build registration
  const registration: RecipientRegistration = {
    script_hash: scriptHash,
    state_locator: {
      policy_id: nftPolicyId,
      asset_name: fromText("state"),
    },
    reference_script_locator: {
      policy_id: nftPolicyId,
      asset_name: fromText("ref"),
    },
    additional_inputs: [],
    recipient_type: { GenericHandler: {} },
    custom_ism: null,
  };
  
  // 5. Register with Hyperlane registry
  await registerWithHyperlane(lucid, registration);
  
  return {
    scriptHash,
    stateUtxo: { txHash, outputIndex: 0 },
    refScriptUtxo: { txHash, outputIndex: 1 },
    registration,
  };
}

async function registerWithHyperlane(
  lucid: Lucid,
  registration: RecipientRegistration,
): Promise<string> {
  // Find registry UTXO
  const registryNft = HYPERLANE_CONFIG.registry_state_nft;
  const registryUtxos = await lucid.utxosAtWithUnit(
    HYPERLANE_CONFIG.registry_address,
    registryNft,
  );
  
  if (registryUtxos.length !== 1) {
    throw new Error("Registry UTXO not found");
  }
  
  const registryUtxo = registryUtxos[0];
  const currentDatum = Data.from(registryUtxo.datum!) as RegistryDatum;
  
  // Add new registration
  const newDatum: RegistryDatum = {
    ...currentDatum,
    registrations: [...currentDatum.registrations, registration],
  };
  
  // Find the recipient state UTXO (to prove ownership)
  const recipientUtxos = await lucid.utxosAtWithUnit(
    lucid.utils.credentialToAddress(
      { type: "Script", hash: registration.script_hash },
    ),
    registration.state_locator.policy_id + registration.state_locator.asset_name,
  );
  
  if (recipientUtxos.length !== 1) {
    throw new Error("Recipient state UTXO not found");
  }
  
  // Build registration transaction
  const tx = await lucid
    .newTx()
    // Spend registry to update
    .collectFrom(
      [registryUtxo],
      Data.to({ Register: { registration } }), // Redeemer
    )
    // Spend recipient to prove ownership (recreate unchanged)
    .collectFrom(
      [recipientUtxos[0]],
      Data.to({ ContractAction: { action: Data.void() } }),
    )
    // Registry continuation with new entry
    .payToContract(
      HYPERLANE_CONFIG.registry_address,
      { inline: Data.to(newDatum) },
      registryUtxo.assets,
    )
    // Recipient continuation (unchanged)
    .payToContract(
      recipientUtxos[0].address,
      { inline: recipientUtxos[0].datum! },
      recipientUtxos[0].assets,
    )
    // Attach reference scripts
    .readFrom([
      await getRefScriptUtxo(lucid, HYPERLANE_CONFIG.registry_ref_script_nft),
      await getRefScriptUtxo(lucid, registration.reference_script_locator!),
    ])
    .complete();
  
  const signedTx = await tx.sign().complete();
  const txHash = await signedTx.submit();
  
  console.log(`Registration tx submitted: ${txHash}`);
  await lucid.awaitTx(txHash);
  console.log(`Registration confirmed!`);
  
  return txHash;
}
```

---

## Reference Script Holder Patterns

### Pattern 1: Deployer Address + NFT (Recommended)

```
┌─────────────────────────────────────────┐
│ Reference Script UTXO                   │
│                                         │
│ Address: deployer's pub key address     │
│ Value: min_ada + NFT(policy, "ref")     │
│ Datum: None                             │
│ Reference Script: <validator code>      │
└─────────────────────────────────────────┘
```

**Finding it:** Query by NFT (via registry locator)

**Security:** Deployer could spend it (destroying the reference), but:
- NFT makes it easy to find
- Deployer is incentivized to keep it (their app breaks otherwise)
- Could use multisig address for extra protection

### Pattern 2: "Always Fails" Script Address

```aiken
// validators/always_fails.ak

validator {
  fn always_fails(_d: Data, _r: Data, _ctx: ScriptContext) -> Bool {
    False
  }
}
```

```
┌─────────────────────────────────────────┐
│ Reference Script UTXO                   │
│                                         │
│ Address: always_fails script address    │
│ Value: min_ada + NFT(policy, "ref")     │
│ Datum: None                             │
│ Reference Script: <validator code>      │
└─────────────────────────────────────────┘
```

**Finding it:** Query by NFT

**Security:** Cannot be spent (script always fails) → immutable forever

**Downside:** ADA is locked forever (acceptable for important contracts)

### Pattern 3: Hyperlane-Managed Reference Script Registry

Hyperlane could maintain a dedicated "reference script holder" contract:

```aiken
// validators/ref_script_holder.ak

type RefScriptHolderDatum {
  script_hash: ScriptHash,
  owner: VerificationKeyHash,
}

validator {
  fn ref_script_holder(
    datum: RefScriptHolderDatum,
    redeemer: RefScriptHolderRedeemer,
    ctx: ScriptContext,
  ) -> Bool {
    when redeemer is {
      // Only owner can withdraw (destroy reference)
      Withdraw -> {
        list.has(ctx.transaction.extra_signatories, datum.owner)
      }
    }
  }
}
```

**Finding it:** Query Hyperlane ref script holder address, filter by script_hash in datum

**Security:** Owner-controlled withdrawal

---

## Summary

| Component | Locator | Purpose |
|-----------|---------|---------|
| `state_locator` | NFT(policy, "state") | Find recipient's current state UTXO |
| `reference_script_locator` | NFT(policy, "ref") | Find UTXO containing validator code |
| Additional inputs | NFT per input | Find vaults, pools, etc. |

The relayer flow:
1. Query registry → get locators
2. Query chain by NFTs → get current UTXO refs
3. Build tx with reference inputs for scripts, script inputs for state
4. Submit (retry on contention)

This design ensures the relayer can construct complete transactions for any registered recipient without recipient-specific off-chain code.
