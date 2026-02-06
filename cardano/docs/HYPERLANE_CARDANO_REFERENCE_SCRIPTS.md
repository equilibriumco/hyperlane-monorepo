# Hyperlane Cardano: Reference Script UTXO Handling

## Problem Statement

When spending a script UTXO on Cardano, the transaction needs access to the validator code. There are two mechanisms:

1. **Witness script** - Include the full script in the transaction (expensive, bloats tx size)
2. **Reference script** - Point to a UTXO that contains the script (cheaper, preferred post-Vasil)

The Hyperlane relayer needs to know: **"Where is the UTXO containing the recipient's validator code?"**

This document describes how to handle reference script discovery in the Hyperlane Cardano integration.

---

## Solution: Configuration-Based Discovery

Reference scripts are resolved without any on-chain registry:

- **Core contracts** (mailbox, ISM): Reference script UTXOs are configured in the relayer's `ConnectionConf` (e.g., `mailbox_reference_script_utxo`, `ism_reference_script_utxo`).
- **Warp routes**: A shared reference script UTXO is configured via `warp_route_reference_script_utxo` in the relayer config. All warp route instances of the same validator share the same reference script.

No registry lookup or explicit registration step is needed.

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

## Configuration

### Relayer ConnectionConf

The relayer's Cardano connection configuration includes reference script UTXO locations:

```
cardano_connection {
  ...
  mailbox_reference_script_utxo: "<tx_hash>#<output_index>"
  ism_reference_script_utxo: "<tx_hash>#<output_index>"
  warp_route_reference_script_utxo: "<tx_hash>#<output_index>"
}
```

These point to UTXOs that contain the validator code as a reference script. They are set once at deployment time and remain stable (reference script UTXOs are never spent).

---

## Relayer UTXO Discovery Flow

```
Message arrives: { recipient: 0x01000000{nft_policy}, body: ... }
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 1. Resolve recipient NFT policy from address    │
│                                                 │
│    The recipient address encodes the NFT policy │
│    used to locate the state UTXO.               │
└─────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 2. Discover state UTXO by NFT query             │
│                                                 │
│    State UTXO:                                  │
│    └─ Query: UTXO containing NFT(nft_policy)    │
│    └─ Found: tx_abc#0 at script address         │
│    └─ Read datum for config, ISM, etc.          │
└─────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 3. Load reference script UTXO from config       │
│                                                 │
│    Reference script UTXO:                       │
│    └─ From: warp_route_reference_script_utxo    │
│              in relayer ConnectionConf          │
│    └─ Contains the warp route validator code    │
└─────────────────────────────────────────────────┘
                            │
                            ▼
┌─────────────────────────────────────────────────┐
│ 4. Build Transaction                            │
│                                                 │
│    Reference Inputs (read-only):                │
│    ├─ Warp route reference script UTXO          │
│    ├─ ISM UTXO (for verification)               │
│    └─ Mailbox reference script UTXO             │
│                                                 │
│    Script Inputs (spent):                       │
│    ├─ Mailbox UTXO + Process redeemer           │
│    ├─ tx_abc#0 (state) + HandleMessage redeemer │
│    └─ Vault UTXO + Release redeemer (if needed) │
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
│ 5. Sign and Submit                              │
│                                                 │
│    On UTXO contention failure:                  │
│    └─ Re-run from step 2 with fresh queries     │
└─────────────────────────────────────────────────┘
```

---

## Relayer Implementation Overview

The relayer resolves UTXOs for transaction building as follows:

1. **Recipient state UTXO**: Queried via NFT policy extracted from the recipient address (`0x01000000{nft_policy}`). The relayer queries Blockfrost for the UTXO containing that NFT.

2. **Reference script UTXOs**: Loaded from the relayer's `ConnectionConf`:

   - `mailbox_reference_script_utxo` -- mailbox validator code
   - `ism_reference_script_utxo` -- ISM validator code
   - `warp_route_reference_script_utxo` -- warp route validator code

3. **Additional inputs** (vaults, etc.): Discovered from the warp route datum, which contains vault locator information.

No registry contract or registration transaction is involved in this flow.

---

## Deployment

When deploying a warp route, the deployment transaction produces two outputs:

- **Output #0**: State UTXO at the script address, containing the state NFT and datum
- **Output #1**: Reference script UTXO, containing the reference script NFT (`726566` = "ref" in hex) and the validator code

The reference script NFT uses the same minting policy as the state NFT but with asset name `726566`. This UTXO is never spent -- it is only ever used as a reference input.

After deployment, the relayer is configured with `warp_route_reference_script_utxo` pointing to the reference script UTXO. Multiple warp route instances sharing the same validator code can share a single reference script UTXO.

---

## Reference Script Holder Patterns

### Deployer Address + NFT (Current Approach)

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

**Finding it:** Configured in relayer's `ConnectionConf` by UTXO reference

**Security:** Deployer could spend it (destroying the reference), but:

- NFT makes it easy to find if the UTXO reference changes
- Deployer is incentivized to keep it (their app breaks otherwise)
- Could use multisig address for extra protection

---

## Summary

| Component                   | Discovery Method                          | Purpose                                  |
| --------------------------- | ----------------------------------------- | ---------------------------------------- |
| Recipient state UTXO        | NFT query from recipient address          | Find recipient's current state and datum |
| Mailbox reference script    | `mailbox_reference_script_utxo` config    | Mailbox validator code                   |
| ISM reference script        | `ism_reference_script_utxo` config        | ISM validator code                       |
| Warp route reference script | `warp_route_reference_script_utxo` config | Warp route validator code                |

The relayer flow:

1. Extract NFT policy from recipient address
2. Query chain by NFT policy to find state UTXO
3. Load reference script UTXOs from config
4. Build tx with reference inputs for scripts, script inputs for state
5. Submit (retry on contention)

This design avoids the need for any on-chain registry contract. Recipients are resolved directly via NFT queries, and reference scripts are configured statically in the relayer.
