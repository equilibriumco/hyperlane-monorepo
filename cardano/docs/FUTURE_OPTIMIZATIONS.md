# Future Optimizations

This document captures potential optimizations for the Hyperlane-Cardano integration that are not required for initial production deployment but may be valuable as usage scales.

## Parallel Message Processing (Minting Policy Architecture)

**Status:** Design Complete | Implementation: Future
**Priority:** When throughput becomes a bottleneck
**Estimated Effort:** ~15-20 days

### Problem

The current architecture creates UTXO contention that limits throughput:

```
Current: Processing a message requires spending 3 UTXOs
  - Mailbox UTXO (spent) → validator runs
  - ISM UTXO (spent) → validator runs
  - Recipient UTXO (spent) → validator runs

Result: Only ~1 message per block (~3 messages/minute)
```

Since each UTXO can only be spent once per block, messages are processed sequentially even when destined for different recipients.

### Solution: Convert to Minting Policies

**Key insight:** Minting policies run when tokens are minted - without requiring any UTXO to be spent.

**New architecture:**

```
Transaction (Process Message):
  Mint: ISM Verification NFT      → ISM minting policy validates signatures
  Mint: Process Authorization NFT → Mailbox minting policy validates message
  Mint: Processed Message NFT     → Replay protection (existing)

  Reference Input: ISM Config UTXO (read validator set, threshold)
  Reference Input: Mailbox Config UTXO (read domain, default_ism)

  Input: Recipient UTXO (spent) → only recipient validator runs
  Output: Recipient UTXO (updated state)
```

**Contention eliminated for:**
- Mailbox (minting policy, no UTXO)
- ISM (minting policy, no UTXO)

**Contention remains for:**
- Recipient (unavoidable - recipient state must update)

### Throughput Improvement

| Scenario | Current | Optimized |
|----------|---------|-----------|
| 10 messages to 10 recipients | ~3.3 min | ~20 sec |
| 10 messages to 1 recipient | ~3.3 min | ~3.3 min |

### Security Model

Security is maintained through minting policy validation:

1. **ISM Minting Policy** validates:
   - Threshold signatures from validator set
   - Checkpoint matches message_id
   - Reads validator config from reference input

2. **Mailbox Minting Policy** validates:
   - Message destination matches local domain
   - Message ID correctly computed (keccak256)
   - ISM verification NFT is minted (proves signatures valid)
   - Recipient is spent in transaction
   - Processed message NFT minted (replay protection)

3. **Recipient Validator** validates:
   - Mailbox process NFT is minted (proves mailbox validated)
   - Handles message (existing logic)

### Components to Implement

| Component | Description |
|-----------|-------------|
| `ism_verify.ak` | New minting policy for signature verification |
| `mailbox_process.ak` | New minting policy for message processing |
| `mailbox_config.ak` | Spend validator for config UTXO (admin only) |
| `ism_config.ak` | Spend validator for ISM config (admin only) |
| Recipient updates | Check minting policy instead of mailbox spend |
| Relayer updates | Parallel transaction building and submission |

### Migration Path

1. Deploy new minting policies alongside existing validators
2. Update recipients incrementally (registry tracks version)
3. Relayer detects recipient version, uses appropriate transaction format
4. Eventually deprecate old architecture

### When to Implement

Consider implementing when:
- Message volume exceeds ~100/hour sustained
- Relayer queue depth regularly exceeds 10 messages
- User complaints about delivery latency
- Planning for high-volume applications (DEX, gaming, etc.)

### References

- [Cardano Minting Policies](https://aiken-lang.org/language-tour/minting-policies)
- [Current Architecture](./DESIGN.md)

---

## Other Potential Optimizations

### Dispatch Batching

**Problem:** Outgoing messages (dispatch) also have contention on mailbox UTXO.

**Solution:** Batch multiple dispatches into single transaction.

**Consideration:** Less critical than incoming - dispatch is typically user-initiated and less frequent. The "poison message" problem is also less severe since users control their own messages.

### Reference Script Caching

**Problem:** Reference scripts fetched from Blockfrost for each transaction.

**Solution:** In-memory cache with TTL, invalidated on UTXO consumption.

**Status:** Documented in Epic 4 (Task 4.3)

### Parallel Blockfrost Queries

**Problem:** Sequential queries that could run in parallel.

**Solution:** Use `tokio::try_join!` for independent queries.

**Status:** Documented in Epic 4 (Task 4.4)

---

## Future Features

These features are part of the Hyperlane specification but are not required for initial Cardano integration. They may be implemented based on user demand.

### Routing ISM

**Status:** Not Planned | Priority: Low
**Reference:** [Hyperlane ISM Documentation](https://docs.hyperlane.xyz/docs/reference/ISM/specify-your-ISM)

**Description:**
The Routing ISM allows specifying different ISMs for different origin domains. For example, messages from Ethereum might use a 3/5 multisig while messages from Avalanche use a 2/3 multisig.

**Cardano Consideration:**
Would require a new `routing_ism.ak` contract that:
- Stores a mapping of origin domain → ISM policy ID
- Delegates verification to the appropriate ISM based on message origin
- Falls back to a default ISM for unmapped domains

**When to implement:** When users need different security levels for different source chains.

### Aggregation ISM

**Status:** Not Planned | Priority: Low
**Reference:** [Hyperlane ISM Documentation](https://docs.hyperlane.xyz/docs/reference/ISM/specify-your-ISM)

**Description:**
The Aggregation ISM requires multiple ISMs to verify a message (AND logic). For example, a message must be verified by BOTH a multisig ISM AND a merkle proof ISM.

**Cardano Consideration:**
Would require:
- New `aggregation_ism.ak` contract
- Mechanism to collect verification proofs from multiple ISMs
- Transaction that mints verification tokens from each sub-ISM

**When to implement:** When users need defense-in-depth security combining multiple verification methods.

### Interchain Accounts (ICA)

**Status:** Not Planned | Priority: Medium
**Reference:** [Hyperlane ICA Documentation](https://docs.hyperlane.xyz/docs/guides/developer-tips/interchain-accounts)

**Description:**
Interchain Accounts allow a contract on one chain to control an account on another chain. For example, a DAO on Ethereum could execute transactions on Cardano via its interchain account.

**Cardano Consideration:**
The eUTXO model makes this complex:
- Would need an `InterchainAccountRouter` contract on Cardano
- Account ownership tied to message origin (origin domain + sender)
- Need to serialize Cardano transactions in messages for remote execution
- Challenge: Cardano transactions are UTXO-based, not account-based

**Architecture Option:**
```
Ethereum DAO → dispatch(cardano, InterchainAccountRouter, calldata)
                            ↓
Cardano InterchainAccountRouter receives message
                            ↓
Router creates/submits transaction on behalf of ICA
```

**When to implement:** When there's demand for cross-chain governance or DAO operations involving Cardano.

### Interchain Query System (IQS)

**Status:** Not Planned | Priority: Low
**Reference:** [Hyperlane Queries](https://docs.hyperlane.xyz/docs/reference/applications/interchain-queries)

**Description:**
Allows querying state from remote chains. For example, an Ethereum contract could query the balance of an address on Cardano.

**Cardano Consideration:**
- Would need query responders that can serialize Cardano state
- Challenge: Cardano state is distributed across UTXOs, not in account storage
- Would need to define queryable state types (token balances, UTXO lookups, etc.)

**When to implement:** When cross-chain applications need to read Cardano state from other chains.

### Warp Route Rate Limiting

**Status:** Not Planned | Priority: Medium
**Reference:** Standard security practice

**Description:**
Rate limiting on warp routes to prevent large-scale exploits. If a vulnerability is discovered, rate limiting can cap losses before human intervention.

**Implementation:**
- Per-token transfer limits (max per transaction, max per time period)
- Circuit breaker that pauses transfers if unusual activity detected
- Admin controls for emergency pause

**When to implement:** Before mainnet deployment with significant TVL
