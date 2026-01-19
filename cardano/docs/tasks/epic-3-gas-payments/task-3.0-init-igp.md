[← Epic 3: Gas Payments](./EPIC.md) | [Epics Overview](../README.md)

# Task 3.0: Initialize IGP Contract
**Status:** ✅ Completed
**Complexity:** Low
**Depends On:** None (PREREQUISITE for all other Epic 3 tasks)

## Objective

Add `init igp` CLI command to initialize the Interchain Gas Paymaster contract on Cardano. This is a prerequisite for all other Epic 3 tasks since they require an initialized IGP to test against.

## Background

The IGP contract exists (`cardano/contracts/validators/igp.ak`) but is not initialized on testnet. Like other Hyperlane contracts (Mailbox, ISM, Registry), it requires:
- A one-shot state NFT for UTXO identification
- An initial datum with configuration
- Registration in `deployment_info.json`

Without initialization, we cannot:
- Test CLI commands (Task 3.2)
- Verify the Rust indexer (Task 3.1)
- Run relayer integration tests (Task 3.3)
- Perform E2E testing (Task 3.4)

## IGP Datum Structure

From `cardano/contracts/lib/types.ak`:

```aiken
pub type IgpDatum {
  owner: VerificationKeyHash,           // Who can configure oracles
  beneficiary: ByteArray,               // Who receives claimed fees
  gas_oracles: List<(Domain, GasOracleConfig)>,  // Price config per destination
  default_gas_limit: Int,               // Default gas if not specified
}

pub type GasOracleConfig {
  gas_price: Int,                       // Destination chain gas price
  token_exchange_rate: Int,             // ADA to destination token rate
}
```

## Requirements

### 1. Add `build_igp_datum()` to CBOR Utils

**File:** `cardano/cli/src/utils/cbor.rs`

```rust
/// Build IGP datum CBOR
/// Structure: Constr 0 [owner, beneficiary, gas_oracles, default_gas_limit]
pub fn build_igp_datum(
    owner_pkh: &str,                      // 28 bytes hex (verification key hash)
    beneficiary: &str,                    // 28 bytes hex (verification key hash)
    gas_oracles: &[(u32, u64, u64)],      // (domain, gas_price, exchange_rate)
    default_gas_limit: u64,
) -> Result<Vec<u8>>
```

CBOR structure:
```
Constr 0 [
  owner: ByteArray (28 bytes),
  beneficiary: ByteArray (28 bytes),
  gas_oracles: List<(Int, Constr 0 [Int, Int])>,
  default_gas_limit: Int
]
```

### 2. Add `Igp` Variant to `InitCommands`

**File:** `cardano/cli/src/commands/init.rs`

```rust
/// Initialize the IGP (Interchain Gas Paymaster) contract
Igp {
    /// Beneficiary address for claimed fees (defaults to signer's pkh)
    #[arg(long)]
    beneficiary: Option<String>,

    /// Default gas limit for messages
    #[arg(long, default_value = "200000")]
    default_gas_limit: u64,

    /// Gas oracle config: "domain:gas_price:exchange_rate" (repeatable)
    #[arg(long = "oracle")]
    oracles: Vec<String>,

    /// UTXO to use for minting state NFT (tx_hash#index)
    #[arg(long)]
    utxo: Option<String>,

    /// Dry run - show what would be done without submitting
    #[arg(long)]
    dry_run: bool,
},
```

### 3. Implement `init_igp()` Function

**Flow:**

```
1. Load signing key → derive owner_pkh
2. Determine beneficiary:
   - If --beneficiary provided: parse and use
   - Else: use owner_pkh (signer receives fees)
3. Parse --oracle flags into (domain, gas_price, exchange_rate) tuples
4. Get UTXOs from wallet, find suitable one (>= 10 ADA, no assets)
5. Encode output reference for one-shot NFT parameter
6. Apply state_nft parameter → get NFT policy ID
7. Get IGP script address from deployment_info.json
8. Build IGP datum with:
   - owner: owner_pkh
   - beneficiary: beneficiary_pkh
   - gas_oracles: parsed from --oracle flags (empty list if none)
   - default_gas_limit: from --default-gas-limit
9. Build transaction:
   - Input: selected UTXO
   - Mint: state NFT with policy from step 6
   - Output: IGP address + 5 ADA + state NFT + datum
   - Collateral: separate UTXO
10. Sign with Cardano signing key
11. Submit to network
12. Update deployment_info.json:
    - igp.stateNftPolicy = NFT policy ID
    - igp.stateNft = { policyId, assetName, seedUtxo }
    - igp.stateUtxo = "tx_hash#0"
    - igp.initTxHash = tx_hash
    - igp.initialized = true
```

### 4. Wire Up in Execute Match

```rust
InitCommands::Igp { beneficiary, default_gas_limit, oracles, utxo, dry_run } =>
    init_igp(ctx, beneficiary, default_gas_limit, oracles, utxo, dry_run).await,
```

## CLI Interface

```bash
# Basic initialization (owner = beneficiary = signer, no oracles)
hyperlane-cardano init igp

# With default gas limit
hyperlane-cardano init igp --default-gas-limit 200000

# With Fuji oracle configured
hyperlane-cardano init igp \
  --default-gas-limit 200000 \
  --oracle "43113:25000000000:1000000"

# With multiple oracles
hyperlane-cardano init igp \
  --oracle "43113:25000000000:1000000" \
  --oracle "11155111:30000000000:1200000"

# With custom beneficiary (different from owner)
hyperlane-cardano init igp \
  --beneficiary addr_test1qz... \
  --default-gas-limit 200000

# Dry run to preview
hyperlane-cardano init igp --dry-run
```

### Oracle Format

`--oracle "domain:gas_price:exchange_rate"`

| Field | Description | Example |
|-------|-------------|---------|
| domain | Destination domain ID | 43113 (Fuji) |
| gas_price | Gas price in destination native units | 25000000000 (25 gwei) |
| exchange_rate | ADA to destination token rate (scaled) | 1000000 |

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/init.rs` | Add `Igp` command variant, `init_igp()` function |
| `cardano/cli/src/utils/cbor.rs` | Add `build_igp_datum()` function |
| `cardano/deployments/preview/deployment_info.json` | Auto-updated after init |

## Expected Output

```
Initializing IGP contract...
  Owner: a1b2c3d4e5f6...
  Beneficiary: a1b2c3d4e5f6...
  Default Gas Limit: 200,000
  Gas Oracles: 1 configured
    - Domain 43113: gas_price=25000000000, exchange_rate=1000000

Applying state_nft parameter...
  Input UTXO: abc123...#0
  State NFT Policy ID: 7f8e9d0c1a2b...

Building transaction...
  IGP Address: addr_test1wpqlvpjt4xvfmtuwltkq0yekqs342tfdevh3dxgj2j3fqtg5pzvu0
  Output: 5 ADA + state NFT + datum
  TX Hash: def456...

Signing transaction...
  Signed TX size: 892 bytes

Submitting transaction...
✓ Transaction submitted!
  TX Hash: def456789abc...
  Explorer: https://preview.cardanoscan.io/transaction/def456789abc...

✓ Deployment info updated
  IGP State NFT Policy: 7f8e9d0c1a2b...
  IGP State UTXO: def456789abc...#0
  IGP Initialized: true
```

## Testing

### Manual Testing on Preview

1. Ensure wallet has >= 30 ADA (12 for reference script, 10 for init, 5 for collateral, buffer for fees)

2. Deploy IGP reference script (required before init):
   ```bash
   hyperlane-cardano deploy reference-script --script igp --dry-run
   # If dry-run looks good:
   hyperlane-cardano deploy reference-script --script igp
   ```
   This creates a UTXO containing the IGP validator script that can be referenced by future transactions, saving transaction fees.

3. Run init dry-run:
   ```bash
   hyperlane-cardano init igp --dry-run
   ```

4. If dry-run looks good, run actual init:
   ```bash
   hyperlane-cardano init igp --default-gas-limit 200000
   ```

5. Verify on explorer that UTXO exists at IGP address

6. Verify `deployment_info.json` updated correctly

7. Run `hyperlane-cardano init status` to confirm

### Verification Checklist

- [x] Reference script deployed successfully
- [x] `deployment_info.json` has `igp.referenceScriptUtxo` set
- [x] Init transaction submitted successfully
- [x] State UTXO visible on CardanoScan at IGP address
- [x] State NFT present in UTXO
- [x] Datum correctly formed (can decode with Blockfrost)
- [x] `deployment_info.json` has:
  - `igp.initialized = true`
  - `igp.stateNftPolicy` set
  - `igp.stateUtxo` set
  - `igp.referenceScriptUtxo` set
- [x] `init status` shows IGP as initialized

## Definition of Done

- [x] `build_igp_datum()` implemented and tested
- [x] `init igp` command added to CLI
- [x] Dry-run mode works correctly
- [x] Successfully initialized on Preview testnet
- [x] `deployment_info.json` updated correctly
- [x] Documentation updated

## Acceptance Criteria

1. `init igp` command creates valid IGP UTXO on-chain
2. State NFT minted and included in output
3. Datum matches expected structure
4. Gas oracles correctly encoded
5. Deployment info persisted for other tasks to use

## Completion Notes

**Completed:** 2025-01-19

### Deployment Details (Preview Testnet)

| Item | Value |
|------|-------|
| IGP Address | `addr_test1wpqlvpjt4xvfmtuwltkq0yekqs342tfdevh3dxgj2j3fqtg5pzvu0` |
| State NFT Policy | `0412bff6c732f13b2412b72a7dd58faac8fea7b76d38fdba2987bfe2` |
| State UTXO | `173ba82ad21dcddd2880f48db4da221dec0982a77fe288c56e54290a2cc0b140#0` |
| Reference Script UTXO | `f585118df2ab8a4616f078348b010c5932727cda37d60125160cfa1a29364b9a#0` |
| Init TX | [173ba82ad21dcddd2880f48db4da221dec0982a77fe288c56e54290a2cc0b140](https://preview.cardanoscan.io/transaction/173ba82ad21dcddd2880f48db4da221dec0982a77fe288c56e54290a2cc0b140) |

### Implementation Summary

1. **`build_igp_datum()`** added to `cardano/cli/src/utils/cbor.rs` with 5 unit tests
2. **`Igp` variant** added to `InitCommands` enum in `cardano/cli/src/commands/init.rs`
3. **`init_igp()`** function implemented with full transaction building flow
4. **`parse_oracle_config()`** helper added with 9 unit tests
