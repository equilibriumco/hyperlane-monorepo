# Interchain Gas Paymaster (IGP) Guide

This guide explains how the Interchain Gas Paymaster works for Cardano cross-chain messages, including oracle configuration, gas cost modeling, and integration details.

## Cardano Gas Cost Model

Cardano transaction costs differ fundamentally from EVM gas:

```
cardano_tx_fee = min_fee_a * tx_size_bytes + min_fee_b + script_execution_cost + ref_script_cost
```

Where:
- `min_fee_a` = 44 lovelace/byte (protocol parameter)
- `min_fee_b` = 155,381 lovelace
- `script_execution_cost` depends on script complexity
- `ref_script_cost` = 15 lovelace/byte for reference scripts used in the TX (Conway era)

For message delivery transactions, costs depend on the **recipient type**:

### Warp Route Recipients (`0x01` prefix)

Warp routes validate messages independently — no `verified_message` UTXO is created.

| Component | Cost | Nature |
|-----------|------|--------|
| Processed-marker UTXO (NFT + datum) | ~1,500,000 lovelace | Fixed |
| Script execution fee | ~95,000-133,000 lovelace | Fixed |
| Base TX skeleton (~8KB) | ~330,000-346,000 lovelace | Fixed |
| Reference script fee (15 lovelace/byte) | ~150,000 lovelace | Fixed |
| Message body in TX | ~44 lovelace/byte | Variable |

**Warp route total = ~2,100,000 fixed + 44 * body_size variable**

### Script Recipients (`0x02` prefix)

Script recipients (e.g. greeting contract) receive a `verified_message` UTXO containing the full message body as an inline datum. This UTXO's minimum ADA scales with `coins_per_utxo_byte` (4,310 lovelace/byte):

| Component | Cost | Nature |
|-----------|------|--------|
| Processed-marker UTXO (NFT + datum) | ~1,500,000 lovelace | Fixed |
| Verified-message UTXO (NFT + datum) | ~1,700,000 + 4,310 * body_size | Variable |
| Script execution fee | ~95,000-133,000 lovelace | Fixed |
| Base TX skeleton (~8KB) | ~330,000-346,000 lovelace | Fixed |
| Reference script fee (15 lovelace/byte) | ~150,000 lovelace | Fixed |
| Message body in TX | ~44 lovelace/byte | Variable |

**Script recipient total = ~3,800,000 fixed + 4,354 * body_size variable**

The dominant cost for script recipients is the `verified_message` UTXO — for a 5,000-byte body it requires ~23.3 ADA locked in the output.

Note: TX fees use Blockfrost evaluate when available (Conway-era fee formula including reference script costs), with a body-size-aware static fallback.

## Oracle Configuration

### Mapping Cardano Costs to the EVM IGP Model

For Cardano as a destination, we map:

| IGP Parameter | Value | Meaning |
|---------------|-------|---------|
| `gasPrice` | 44 | Cardano's `min_fee_a` (lovelace per byte) |
| `gasOverhead` | 86,000 | Fixed base costs / 44 (see [gasOverhead](#understanding-gasoverhead)) |
| `gasLimit` | varies by recipient type | Variable cost passed by caller (see [Dispatching to Cardano](#dispatching-to-cardano-from-evm)) |
| `tokenExchangeRate` | market-dependent | Converts lovelace cost to source chain native token |

### Fuji IGP (for Cardano destination, domain 2003)

Two contracts need configuration:

**StorageGasOracle** (`setRemoteGasDataConfigs`):

```bash
cast send $FUJI_STORAGE_GAS_ORACLE \
  "setRemoteGasDataConfigs((uint32,uint128,uint128)[])" \
  "[(2003, <tokenExchangeRate>, 44)]"
```

**IGP** (`setDestinationGasConfigs`):

```bash
cast send $FUJI_IGP \
  "setDestinationGasConfigs((uint32,(address,uint96))[])" \
  "[(2003, ($FUJI_STORAGE_GAS_ORACLE, 86000))]"
```

### Cardano IGP (for Fuji destination, domain 43113)

```bash
hyperlane-cardano igp set-oracle \
  --domain 43113 \
  --gas-price 1000000000 \
  --exchange-rate 34 \
  --gas-overhead 100000
```

| Parameter | Value | Meaning |
|-----------|-------|---------|
| `gas_price` | 1,000,000,000 | 1 gwei (Fuji gas price) |
| `exchange_rate` | 34 | ADA-to-AVAX rate × (1e18/1e6) / 1e12 |
| `gas_overhead` | 100,000 | ~80K EVM `Mailbox.process()` base cost + 25% margin |

### Exchange Rate Calibration

Exchange rates must account for decimal differences between chains:

**Cardano → Fuji** (Cardano IGP):

```
exchange_rate = market_rate_ada_per_avax * (dest_decimals / src_decimals) / scale_factor
             = market_rate * (1e18 / 1e6) / 1e12
             = market_rate (approximately)
```

Example: 1 AVAX = 33.52 ADA → `exchange_rate = 34` (rounded up)

**Fuji → Cardano** (Fuji IGP):

```
tokenExchangeRate = (1 / market_rate) * (src_decimals / dest_decimals) * TOKEN_EXCHANGE_RATE_SCALE
                  = (1 / market_rate) * (1e6 / 1e18) * 1e10
                  = 1e22 / (market_rate * 1e6)
```

Example: 1 AVAX = 33.52 ADA → `tokenExchangeRate = 1e22 / 33.52 ≈ 2.983e20`

## Understanding gasOverhead

The `gasOverhead` parameter covers the **fixed base costs** of delivering a message to Cardano, independent of the message body size. It is set per-destination on the origin chain's IGP.

### How gasOverhead is calculated

On Cardano, every message delivery incurs these fixed costs:

| Component | Lovelace | Description |
|-----------|----------|-------------|
| TX base fee | ~550,000 | `min_fee_b` + base TX skeleton × `min_fee_a` |
| Processed-marker UTXO | ~1,500,000 | NFT + datum, required for all recipients |
| Verified-message UTXO base | ~1,700,000 | NFT + datum overhead (script recipients only) |
| Reference script fee | ~150,000 | 15 lovelace/byte for ref scripts |

We size `gasOverhead` for the worst case (script recipients):

```
gasOverhead = total_fixed_cost / gasPrice
            = (550,000 + 1,500,000 + 1,700,000) / 44
            = 3,750,000 / 44
            ≈ 85,227
            → rounded to 86,000
```

For warp routes (no verified-message UTXO), the actual fixed cost is ~2.1M lovelace (48K gas units). Using 86,000 means warp routes overpay by ~1.7 ADA — acceptable for cross-chain transfers.

### How the IGP uses gasOverhead

The IGP's `quoteDispatch` flow automatically adds `gasOverhead` to the caller-provided `gasLimit`:

```
totalGas = gasOverhead + gasLimit
quote = totalGas × gasPrice × tokenExchangeRate / TOKEN_EXCHANGE_RATE_SCALE
```

This means callers only need to specify the **variable** portion (body-size-dependent cost) as `gasLimit`. The fixed costs are handled by `gasOverhead`.

## Dispatching to Cardano from EVM

### Calculating gasLimit

The `gasLimit` covers the **variable** cost of delivering a message, which depends on the recipient type:

**Warp route recipients (`0x01`)** — no `verified_message` UTXO is created, so the only variable cost is the message body increasing the TX size at `min_fee_a` = 44 lovelace/byte:

```
gasLimit = body.length
```

**Script recipients (`0x02`)** — the `verified_message` UTXO stores the full body as an inline datum. Cardano's `coins_per_utxo_byte` (4,310 lovelace) determines the minimum ADA for this UTXO, making the per-byte cost ~99× higher:

```
gasLimit = body.length × 99
```

Why 99 and not `coins_per_utxo_byte` (4,310) directly? Because the IGP multiplies `gasLimit × gasPrice` internally, and `gasPrice` is already set to 44. So gasLimit must be in "gas units" (1 unit = 44 lovelace), not lovelace:

```
actual_cost      = coins_per_utxo_byte × body.length = 4,310 × body.length  (lovelace)
IGP computation  = gasLimit × gasPrice                = gasLimit × 44        (lovelace)

→ gasLimit = 4,310 × body.length / 44 = body.length × 98  (rounded up to 99)
```

### Example: dispatching to a Cardano warp route

```solidity
IMailbox mailbox = IMailbox(mailboxAddress);
uint32 cardanoDomain = 2003;
bytes32 recipient = ...; // 0x01000000 + warp route NFT policy
bytes memory body = abi.encode(recipientAddr, amount); // ~64 bytes

// Step 1: quote — gasLimit = body.length for warp routes
bytes memory metadata = StandardHookMetadata.overrideGasLimit(body.length);
uint256 fee = mailbox.quoteDispatch(cardanoDomain, recipient, body, metadata);

// Step 2: dispatch with the quoted fee
mailbox.dispatch{value: fee}(cardanoDomain, recipient, body, metadata);
```

### Example: dispatching to a Cardano script recipient

```solidity
IMailbox mailbox = IMailbox(mailboxAddress);
uint32 cardanoDomain = 2003;
bytes32 recipient = ...; // 0x02000000 + script hash
bytes memory body = bytes("Hello Cardano");

// Step 1: quote — gasLimit = body.length × 99 for script recipients
uint256 gasLimit = body.length * 99;
bytes memory metadata = StandardHookMetadata.overrideGasLimit(gasLimit);
uint256 fee = mailbox.quoteDispatch(cardanoDomain, recipient, body, metadata);

// Step 2: dispatch with the quoted fee
mailbox.dispatch{value: fee}(cardanoDomain, recipient, body, metadata);
```

### Example: using quoteDispatch without metadata

Without metadata, the IGP defaults to `DEFAULT_GAS_USAGE = 50,000`:

```solidity
// No metadata — uses gasOverhead (86,000) + DEFAULT_GAS_USAGE (50,000) = 136,000 gas
uint256 fee = mailbox.quoteDispatch(cardanoDomain, recipient, body);
mailbox.dispatch{value: fee}(cardanoDomain, recipient, body);
```

This covers:
- Warp routes with bodies up to ~50KB (more than enough, warp bodies are ~64B)
- Script recipients with bodies up to ~505 bytes (50,000 / 99 ≈ 505)

For script recipients with larger bodies, always pass metadata with an explicit gasLimit.

### EVM warp routes (automatic)

Warp routes extend `GasRouter`, which stores a per-destination `destinationGas` and includes it as metadata automatically. End users don't need to calculate anything:

```solidity
// Owner configures once at deployment:
warpRoute.setDestinationGas(2003, 200); // ~200 bytes covers warp message body

// Users just call — gas is quoted and paid automatically:
uint256 fee = warpRoute.quoteGasPayment(2003);
warpRoute.transferRemote{value: fee}(2003, recipient, amount);
```

### From Cardano (dispatching to EVM)

The Cardano CLI's `warp transfer` command automatically computes the gas needed:

```bash
# The CLI calculates gasLimit based on message body size
hyperlane-cardano warp transfer \
  --domain 43113 \
  --recipient 0x... \
  --amount 1000000 \
  --warp-policy $CARDANO_NATIVE_WARP_NFT_POLICY

# Then pay for gas (gasOverhead is added automatically by the oracle)
hyperlane-cardano igp pay-for-gas \
  --message-id 0x... \
  --destination 43113 \
  --gas-limit 150000  # handle() gas on EVM (gasOverhead added by Cardano IGP)
```

## Quote Verification

### Fuji IGP Quote (for Cardano delivery)

```bash
# Quote for a 100-byte warp route message (must include gasOverhead manually)
cast call $FUJI_IGP "quoteGasPayment(uint32,uint256)(uint256)" 2003 86100

# The IGP internally computes:
# quote = gasAmount * gasPrice * tokenExchangeRate / 1e10
# quote = 86100 * 44 * tokenExchangeRate / 1e10
```

Note: `quoteGasPayment` does NOT add gasOverhead — you must include it in the gasAmount.
The full flow through `quoteDispatch` (called by the Mailbox) adds gasOverhead automatically via `destinationGasLimit()`.

### Cardano IGP Quote (for Fuji delivery)

```bash
hyperlane-cardano igp quote --destination 43113 --gas-limit 150000
# Output shows: gasOverhead(100000) + gasLimit(150000) = 250000 total gas
# Payment = 250000 * 1000000000 * 34 / 1e12 = 8500 lovelace
```

## Relayer Gas Payment Enforcement

The relayer config controls how gas payments are enforced:

```json
{
  "gasPaymentEnforcement": [
    {
      "type": "onChainFeeQuoting",
      "gasFraction": "1/1"
    }
  ]
}
```

- `onChainFeeQuoting` with `gasFraction: "1/1"` requires 100% of the quoted gas to be paid
- The relayer indexes `PayForGas` transactions from the IGP contract
- Messages without sufficient payment are skipped until payment is made

## Recipient Type Impact on Costs

The cost model differs significantly between recipient types because of the `verified_message` UTXO that script recipients require.

| | Warp Routes (`0x01`) | Script Recipients (`0x02`) |
|---|---|---|
| `verified_message` UTXO | Not created | Stores full body as inline datum |
| Fixed base cost | ~2.1M lovelace | ~3.8M lovelace |
| Per-byte cost | ~44 lovelace/byte | ~4,354 lovelace/byte |
| Per-byte in gas units | 1 gas unit/byte | ~99 gas units/byte |
| gasLimit formula | `body.length` | `body.length * 99` |
| 100B total cost | ~2.1 ADA | ~4.2 ADA |
| 5,000B total cost | ~2.3 ADA | ~25.6 ADA |

The per-byte cost for script recipients is driven by `coins_per_utxo_byte` (4,310 lovelace), a Cardano protocol parameter that determines minimum ADA for UTXOs with inline data. This is ~99x the `min_fee_a` (44 lovelace/byte) used for TX size fees.

### Implications for gasLimit

- **Warp routes**: Pass `body.length` as gasLimit — the per-byte TX fee (44 lovelace) maps 1:1 to gas units
- **Script recipients**: Pass `body.length * 99` as gasLimit to cover the `verified_message` UTXO cost
- **`DEFAULT_GAS_USAGE` (50,000)**: Sufficient for warp routes (covers ~50KB TX), but only covers ~500 bytes for script recipients

### Relayer Cost Estimation

The relayer's `estimate_process_cost` computes actual per-output minUTxO costs using `coins_per_utxo_byte` and the output's datum size. When Blockfrost evaluate is unavailable, a static fallback scales with body size for script recipients.

## Recalibration

Oracle values should be updated when:
- Market exchange rates change significantly (>10%)
- Cardano protocol parameters change (e.g., `min_fee_a`, `coins_per_utxo_byte`)
- EVM destination gas prices change significantly

### Steps to Recalibrate

1. Get current market rate (e.g., from CoinGecko)
2. Calculate new exchange rates using formulas above
3. Recalculate `gasOverhead`: (script recipient fixed cost in lovelace) / `min_fee_a`
4. Update StorageGasOracle on EVM side
5. Update IGP `setDestinationGasConfigs` with new `gasOverhead`
6. Update Cardano IGP oracle via CLI
7. Verify quotes are reasonable with `quoteGasPayment` / `igp quote`
