# Interchain Gas Paymaster (IGP) Guide

This guide explains how the Interchain Gas Paymaster works for Cardano cross-chain messages, including oracle configuration, gas cost modeling, and integration details.

## Cardano Gas Cost Model

Cardano transaction costs differ fundamentally from EVM gas:

```
cardano_tx_fee = min_fee_a * tx_size_bytes + min_fee_b + script_execution_cost
```

Where:
- `min_fee_a` = 44 lovelace/byte (protocol parameter)
- `min_fee_b` = 155,381 lovelace
- `script_execution_cost` depends on script complexity

For message delivery transactions, the total cost breaks down into:

| Component | Cost | Nature |
|-----------|------|--------|
| Minimum UTXO (with datum) | ~2,000,000 lovelace | Fixed |
| Script execution fee | ~80,000 lovelace | Fixed |
| Base TX skeleton (~8KB) | ~350,000 lovelace | Fixed |
| Message body in TX | ~44 lovelace/byte | Variable |

**Total = ~2,430,000 fixed + 44 * body_size variable**

## Oracle Configuration

### Mapping Cardano Costs to the EVM IGP Model

The EVM IGP calculates quotes as:

```
totalGas = gasOverhead + gasLimit
quote = totalGas * gasPrice * tokenExchangeRate / TOKEN_EXCHANGE_RATE_SCALE
```

Where `TOKEN_EXCHANGE_RATE_SCALE = 1e10`.

For Cardano as a destination, we map:

| IGP Parameter | Value | Meaning |
|---------------|-------|---------|
| `gasPrice` | 44 | Cardano's `min_fee_a` (lovelace per byte) |
| `gasOverhead` | 69,319 | Fixed costs / 44, with 25% safety margin |
| `gasLimit` | message body size (bytes) | Variable cost passed by caller |
| `tokenExchangeRate` | market-dependent | Converts lovelace cost to source chain native token |

This means:
- `gasOverhead * gasPrice` = 69,319 * 44 = 3,050,036 lovelace (~3.05 ADA) covers all fixed delivery costs
- `gasLimit * gasPrice` = body_bytes * 44 covers the variable TX size cost
- Callers pass `body.length` as gasLimit, which is intuitive

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
  "[(2003, ($FUJI_STORAGE_GAS_ORACLE, 69319))]"
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

## How Callers Specify Gas

### From EVM (dispatching to Cardano)

Use `StandardHookMetadata.overrideGasLimit()` to pass the message body size:

```solidity
IMailbox mailbox = IMailbox(mailboxAddress);

bytes memory body = abi.encode(recipient, amount);

// gasLimit = body length in bytes
// The IGP adds gasOverhead (fixed costs) automatically
mailbox.dispatch{value: msg.value}(
    2003,                                              // Cardano domain
    recipientAddress,
    body,
    StandardHookMetadata.overrideGasLimit(body.length)
);
```

If no metadata is provided, the IGP defaults to `DEFAULT_GAS_USAGE = 50,000` bytes, which is always sufficient (max Cardano TX size is 16KB).

### From EVM Warp Routes

Warp routes use the `GasRouter` pattern with a fixed `destinationGas` per domain. Since warp transfer message bodies have a predictable fixed size (~100 bytes), a fixed value works:

```solidity
// Set once during configuration
warpRoute.setDestinationGas(2003, 200); // ~200 bytes covers warp message body
```

### From Cardano (dispatching to EVM)

The Cardano CLI's `warp transfer` command automatically computes the gas needed:

```bash
# The CLI calculates gasLimit based on message body size
hyperlane-cardano warp transfer \
  --warp-type native \
  --destination 43113 \
  --recipient 0x... \
  --amount 1000000

# Then pay for gas (gasOverhead is added automatically by the oracle)
hyperlane-cardano igp pay-for-gas \
  --message-id 0x... \
  --destination 43113 \
  --gas-limit 150000  # handle() gas on EVM (gasOverhead added by Cardano IGP)
```

## Quote Verification

### Fuji IGP Quote (for Cardano delivery)

```bash
# Quote for a 100-byte message body
cast call $FUJI_IGP "quoteGasPayment(uint32,uint256)(uint256)" 2003 100

# The IGP internally computes:
# totalGas = gasOverhead(69319) + gasLimit(100) = 69419
# quote = 69419 * 44 * tokenExchangeRate / 1e10
```

Note: `quoteGasPayment` does NOT add gasOverhead. The full flow through `quoteDispatch` (called by the Mailbox) does, via `destinationGasLimit()`.

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

## Recalibration

Oracle values should be updated when:
- Market exchange rates change significantly (>10%)
- Cardano protocol parameters change (e.g., `min_fee_a`)
- EVM destination gas prices change significantly

### Steps to Recalibrate

1. Get current market rate (e.g., from CoinGecko)
2. Calculate new exchange rates using formulas above
3. Update StorageGasOracle on EVM side
4. Update Cardano IGP oracle via CLI
5. Verify quotes are reasonable with `quoteGasPayment` / `igp quote`
