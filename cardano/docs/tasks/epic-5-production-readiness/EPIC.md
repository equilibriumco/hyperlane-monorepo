[← Back to Epics Overview](../README.md)

# Epic 5: Production Readiness

**Priority:** 🟢 Medium
**Status:** ⬜ Not Started
**Phase:** 3 - Production Hardening

## Summary

Add comprehensive monitoring, observability, and operational tooling for production deployments. Includes reorg detection, metrics, dashboards, and alerting.

## Business Value

- Enables production operations with visibility
- Early detection of issues via alerting
- Faster debugging via structured logging
- Operational confidence for mainnet

## Tasks

| # | Task | Status | Depends On | Description |
|---|------|--------|------------|-------------|
| 5.1 | [Reorg Detection](./task-5.1-reorg-detection.md) | ⬜ | - | Detect chain reorganizations |
| 5.2 | [Prometheus Metrics](./task-5.2-prometheus-metrics.md) | ⬜ | - | Export operational metrics |
| 5.3 | [Grafana Dashboards](./task-5.3-grafana-dashboards.md) | ⬜ | 5.2 | Visual dashboards |
| 5.4 | [Alerting](./task-5.4-alerting.md) | ⬜ | 5.2 | Alert rules for incidents |
| 5.5 | [Health Checks](./task-5.5-health-checks.md) | ⬜ | - | Health endpoint for k8s probes |

## Task Details

### 5.1 Reorg Detection

Cardano uses Ouroboros consensus with predictable finality:
- Blocks become final after ~2160 blocks on mainnet
- Preview/testnet has smaller k values

**Implementation:**
```rust
pub struct CardanoReorgDetector {
    provider: CardanoProvider,
    block_cache: HashMap<u32, BlockHash>,
    security_parameter: u32,  // k value
}
```

### 5.2 Prometheus Metrics

Key metrics to export:
```
cardano_messages_processed_total{origin, destination, status}
cardano_transaction_build_duration_seconds{type}
cardano_blockfrost_requests_total{endpoint, status}
cardano_block_height
cardano_mailbox_nonce{direction}
```

### 5.3 Grafana Dashboards

Three dashboards:
1. **Overview**: Message throughput, tx success rate, sync status
2. **Operations**: Failure breakdown, latency percentiles, queues
3. **Debugging**: Recent errors, failed tx details, cache stats

### 5.4 Alerting Rules

Critical alerts:
- Message processing stopped (15min)
- Transaction failure rate > 10%

Warning alerts:
- Blockfrost rate limit low
- Processing latency p95 > 60s

### 5.5 Health Checks

Endpoint for orchestration:
```json
{
  "status": "healthy",
  "components": {
    "blockfrost": { "status": "healthy", "latency_ms": 150 },
    "mailbox": { "status": "healthy", "inbound_nonce": 42 }
  }
}
```

## File Structure

```
cardano/monitoring/
├── dashboards/
│   ├── cardano-overview.json
│   ├── cardano-operations.json
│   └── cardano-debugging.json
├── alerts/
│   └── cardano-alerts.yaml
└── README.md
```

## Definition of Done

- [ ] Reorg detection implemented and tested
- [ ] Prometheus metrics exported
- [ ] Grafana dashboards created
- [ ] Alert rules defined
- [ ] Health endpoint implemented
- [ ] Documentation complete

## Acceptance Criteria

1. Reorgs detected and logged
2. Metrics visible in Prometheus
3. Dashboards functional in Grafana
4. Alerts fire correctly in test scenarios
5. Health endpoint used by k8s probes
