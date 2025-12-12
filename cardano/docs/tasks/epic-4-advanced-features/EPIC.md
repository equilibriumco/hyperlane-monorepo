[← Back to Epics Overview](../README.md)

# Epic 4: Advanced Features

**Priority:** 🟢 Medium
**Status:** ⬜ Not Started
**Phase:** 2 - Feature Completion

## Summary

Implement advanced features including per-recipient custom ISM and performance optimizations. These enhance the system but are not blocking for basic functionality.

## Business Value

- Per-recipient ISM: Allows apps to customize their security model
- Performance: Reduces latency and API costs for high-volume usage

## Tasks

| # | Task | Status | Depends On | Description |
|---|------|--------|------------|-------------|
| 4.1 | [Per-Recipient ISM](./task-4.1-per-recipient-ism.md) | ⬜ | - | Custom ISM per recipient |
| 4.2 | [NFT Lookups](./task-4.2-nft-lookups.md) | ⬜ | - | O(1) recipient lookups via NFT |
| 4.3 | [Ref Script Cache](./task-4.3-ref-script-cache.md) | ⬜ | - | Cache reference script UTXOs |
| 4.4 | [Parallel Queries](./task-4.4-parallel-queries.md) | ⬜ | - | Parallelize Blockfrost calls |

## Task Details

### 4.1 Per-Recipient ISM

**Current State:** Mailbox always uses default ISM, ignoring recipient's custom ISM setting.

**Change Required:**
```aiken
// In mailbox.ak:290-298
fn get_recipient_ism(
  recipient: HyperlaneAddress,
  default_ism: ScriptHash,
  tx: Transaction,
  registry_input: Input,  // NEW: look up custom ISM
) -> ScriptHash
```

### 4.2 NFT-Based Lookups

**Current State:** O(n) iteration through registry dictionary.

**Solution:** Mint NFT with script hash as token name, query by asset directly.

```rust
// O(1) lookup via Blockfrost asset API
async fn get_recipient_by_nft(&self, script_hash: &H256) -> Result<RecipientInfo> {
    let asset_id = format!("{}{}", self.registry_policy_id, hex::encode(script_hash));
    let utxo = self.blockfrost.get_asset_utxo(&asset_id).await?;
    // Parse and return
}
```

### 4.3 Reference Script Cache

**Problem:** Reference scripts fetched from Blockfrost for each transaction.

**Solution:** In-memory cache with TTL, invalidated on UTXO consumption.

### 4.4 Parallel Queries

**Problem:** Sequential Blockfrost queries that could run in parallel.

**Solution:** Use `tokio::try_join!` for independent queries.

## Performance Targets

| Metric | Current | Target |
|--------|---------|--------|
| Recipient lookup | O(n) | O(1) |
| Transaction build time | ~2s | ~500ms |
| Blockfrost calls per tx | ~8 | ~4 |

## Definition of Done

- [ ] Mailbox uses recipient's custom ISM when set
- [ ] NFT-based recipient lookups implemented
- [ ] Reference scripts cached in memory
- [ ] Independent queries parallelized
- [ ] Benchmark shows measurable improvement
- [ ] No regression in correctness

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| Cache staleness | Medium | Conservative TTL, refresh on failure |
| NFT migration complexity | Low | Gradual rollout, backwards compatible |
| Parallel query race conditions | Low | Proper async handling |

## Acceptance Criteria

1. Custom ISM honored for recipients that set it
2. Recipient lookups are O(1) via NFT
3. Transaction building is measurably faster
4. All existing tests pass
