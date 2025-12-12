[← Epic 5: Production Readiness](./EPIC.md) | [Epics Overview](../README.md)

# Task 5.2: Prometheus Metrics
**Status:** ⬜ Not Started
**Complexity:** Medium
**Depends On:** None

## Objective

Export Prometheus metrics for Cardano operations.

## Metrics to Implement

### Message Processing
- `cardano_messages_processed_total{origin, destination, status}` - Counter
- `cardano_message_processing_duration_seconds{origin}` - Histogram
- `cardano_messages_pending{destination}` - Gauge

### Transaction Building
- `cardano_transactions_total{type, status}` - Counter
- `cardano_transaction_build_duration_seconds{type}` - Histogram
- `cardano_transaction_confirmation_duration_seconds{type}` - Histogram
- `cardano_transaction_failures_total{reason}` - Counter

### Blockfrost API
- `cardano_blockfrost_requests_total{endpoint, status}` - Counter
- `cardano_blockfrost_request_duration_seconds{endpoint}` - Histogram
- `cardano_blockfrost_rate_limit_remaining` - Gauge

### Chain State
- `cardano_block_height` - Gauge
- `cardano_mailbox_nonce{direction}` - Gauge (inbound, outbound)
- `cardano_registered_recipients_total` - Gauge

## Implementation Approach

Create a metrics module that:
- Registers all metrics with the Prometheus registry
- Provides methods to record events (e.g., `record_message_processed`)
- Integrates with the relayer's existing metrics infrastructure

The relayer already uses Prometheus, so add Cardano metrics to the existing registry.

## Files to Create/Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/metrics.rs` | New metrics module |
| Various chain files | Instrument with metrics calls |

## Testing

- Metrics registered correctly
- Values update on operations
- Labels are correct and consistent

## Definition of Done

- [ ] All metrics exported
- [ ] Integrated with Cardano operations
- [ ] Visible in /metrics endpoint

## Acceptance Criteria

1. Metrics available in Prometheus
2. Values accurate
3. Labels consistent with other chains
