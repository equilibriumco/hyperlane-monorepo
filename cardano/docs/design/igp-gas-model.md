# Cardano IGP Gas Model

How interchain gas payments are priced and enforced for the Cardano ↔ EVM
integration. Covers both directions and the design decisions behind the numbers.

## The core idea

An Interchain Gas Paymaster (IGP) makes the **sender prepay, in the origin
chain's native token, the cost the relayer will spend delivering on the
destination chain**. The oracle converts a destination-cost estimate into an
origin-token payment:

```
originPayment = (destinationGas + gasOverhead) × gasPrice × tokenExchangeRate / SCALE
                └──────── gas units ─────────┘
```

- **gasOverhead** — fixed, per-destination-domain, **recipient-independent** base
  cost (the plumbing of `Mailbox.process` minus the recipient's handler).
- **destinationGas / gasLimit** — **recipient-specific** cost. Warp routes set
  `destinationGas` per domain; other senders set `gasLimit` in the dispatch
  metadata. This is the destination "handler" cost.
- **gasPrice / tokenExchangeRate** — the oracle's price model.

The relayer independently re-estimates the real destination cost at delivery time
and, under the `onChainFeeQuoting` policy, refuses to deliver unless the payment
covers it. So the source chain cannot be tricked by a lowballed gas limit — only
the relayer ultimately gates delivery.

## Sepolia → Cardano (Sepolia IGP charges ETH for Cardano delivery)

Cardano has **no gas metering** — fees are deterministic (size fee + execution
units + `min_fee_b`), and outputs carry a size-dependent **minUTxO**. We model the
whole thing as **synthetic gas denominated 1:1 in lovelace**:

```
gasPrice          = 1                       (1 gas unit = 1 lovelace)
gasLimit          = estimated Cardano delivery cost, in lovelace
tokenExchangeRate = wei-per-lovelace × 1e10 = 1.395e18   (≈ 7171 ADA/ETH)
SCALE             = 1e10                     (standard Solidity IGP)
```

So `originPayment(wei) = lovelace × 1.395e18 / 1e10 = lovelace × 1.394e8`. A
`gasLimit` reads directly as "lovelace of Cardano cost" — no opaque scaling.

### What the relayer estimates (and charges for)

`estimate_process_cost` returns the **full lovelace the relayer fronts and cannot
recover**, not just the ledger fee:

```
estimate = ledger_fee
         + payer_extra              (recipient-output minUTxO for warp mints/releases)
         + verified_message_minUTxO (script recipients only; grows with body size)
```

`payer_extra` is critical: a **synthetic mint** creates a brand-new recipient
token UTxO whose ~1.2 ADA minimum the relayer funds and never gets back. Omitting
it made the relayer deliver mints at a loss. Native/collateral releases usually
fund the recipient minUTxO from the released amount / locked UTxO, so their
`payer_extra ≈ 0`.

A single shared helper (`compute_payer_extra_lovelace`) computes this for both the
TX builder (which funds the outputs) and the estimator (which charges for them),
so the two never diverge.

### Configuration (the proper split)

Put recipient-independent cost in the overhead, recipient-specific cost in
`destinationGas` / sender `gasLimit`:

| Knob | Value | Meaning |
|---|---|---|
| oracle `gasPrice` (domain 2003) | `1` | 1 gas = 1 lovelace |
| oracle `tokenExchangeRate` | `1395000000000000000` | ADA→ETH, decimal-adjusted |
| IGP `gasOverhead` | `1.5 × base_fee` (~`2062550`) | recipient-independent fee, +50% margin |
| warp route `destinationGas` (synthetic-paired) | `1.5 × recipient_minUTxO` (~`1800000`) | the mint's minUTxO |
| warp route `destinationGas` (native/collateral) | `0` | overhead covers it |
| script sender `gasLimit` | `1.5 × (1_720_800 + 4_400 × body_bytes)` lovelace | verified-message minUTxO |

Because `payer_extra` for a synthetic mint is real, the paired Sepolia route
**must** carry a `destinationGas` covering it, or the mint is rejected under
`gasFraction = 1/1`.

## Cardano → Sepolia (Cardano IGP charges ADA for EVM delivery)

Destination is EVM, so gas is **real** and priced the familiar way:

```
gasPrice          = 1_000_000_000   (1 gwei, assumed Sepolia gas price)
tokenExchangeRate = 7171            (1 ETH = 7171 ADA)
gasOverhead       = 155100          (EVM process() base, +50% margin)
SCALE             = 1e12            (custom Aiken IGP; keeps exchangeRate = human 7171)
```

The relayer estimates via `eth_estimateGas`. Send a warp transfer with
`--gas-limit 0` so the payment is `0 + overhead = 155100` gas.

The overhead was retuned from `211000` after measurement: a Sepolia warp release
used ~103.6k gas, so 211000 was collecting ~2.04× rather than the intended 1.5×.

## What the Cardano IGP enforces on-chain

The Aiken validator is stricter than its Solidity counterpart in three ways, all
deliberate:

- **The overhead is added by the contract**, from the oracle entry, in every
  mode. `PayForGas` carries application gas only, so omitting the overhead is
  not a way to pay less — and unlike Solidity's standalone `payForGas`, there is
  no mode that skips it.
- **The paid delta must equal the quote exactly.** Overpayment is rejected
  alongside underpayment, because the IGP's only exit is `Claim` to the
  beneficiary: an overpayment would silently become operator revenue with no
  record that it was a mistake. Rejecting costs the payer a retry.
- **A destination with no oracle is rejected**, not priced by a fallback. The
  previous fallback (price 1, rate 1e6) divided out to zero for any realistic
  gas amount, so a domain enrolled without its oracle was payable for free:
  sends succeeded, nothing was delivered, and nothing failed near the missing
  config.

What it does *not* enforce: nothing links a `PayForGas` to a dispatch. The
mailbox never inspects the IGP, and the IGP only length-checks the message id.
Bundling the two in one transaction is a client convention — which is why an
unpaid dispatch is a perfectly valid transaction that no relayer will carry.

Warp `destination_gas` is likewise advisory: the route validator stores it and
guards updates, but never checks that a transfer paid it. The CLI reads it when
resolving `--gas-limit`.

## Enforcement

Both legs use a single catch-all policy:

```json
{ "type": "onChainFeeQuoting", "gasFraction": "1/2" }   // set to 1/1 in this deployment
```

- `gasFraction` is the **accept/reject floor**: deliver iff
  `paid_gas ≥ gasFraction × liveEstimate`. It is *not* the margin — the relayer
  collects whatever the sender paid.
- The **margin lives in the overhead / destinationGas** (sized to 1.5× real cost).
  `gasFraction = 1/1` means "never deliver below cost"; the +50% is already paid.
- Underpayment is impossible to sneak through: the relayer re-estimates the real
  cost (Blockfrost TX evaluation for Cardano, `eth_estimateGas` for Sepolia) and
  parks the message as `Retry(GasPaymentRequirementNotMet)` if short. Top up with
  `IGP.payForGas(messageId, domain, gasAmount, refund)` to release it.

## Gotchas

- **IGP address**: the warp routes pay the IGP inside their aggregation hook
  (`0x5aC1BCA8`), not the standalone `SEPOLIA_IGP` some configs point at. The
  relayer must index the one that's actually paid, or enforcement misfires.
- **Raw dispatch to a script recipient**: recipient bytes32 = `0x02` + 28-byte
  script hash. The mailbox's required hook is a `StaticProtocolFee` needing
  `--value 1`; gas is paid separately via `IGP.payForGas`.
- **Oracle is static**: `gasPrice` / `tokenExchangeRate` are owner-set, not a live
  feed. Recalibrate as ETH/ADA prices and Cardano protocol params drift.
