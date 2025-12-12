[← Epic 5: Production Readiness](./EPIC.md) | [Epics Overview](../README.md)

# Task 5.3: Grafana Dashboards
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** [Task 5.2](./task-5.2-prometheus-metrics.md)

## Objective

Create Grafana dashboards for Cardano operations visibility.

## Dashboards to Create

### 1. Cardano Overview

**File:** `cardano/monitoring/dashboards/cardano-overview.json`

Panels:
- Message throughput (rate over time)
- Transaction success rate
- Block height and sync status
- Blockfrost API health
- Active alerts summary

### 2. Cardano Operations

**File:** `cardano/monitoring/dashboards/cardano-operations.json`

Panels:
- Transaction failure breakdown by reason
- Processing latency percentiles (p50, p95, p99)
- Pending message queue depth
- Fee trends over time
- Blockfrost rate limit usage

### 3. Cardano Debugging

**File:** `cardano/monitoring/dashboards/cardano-debugging.json`

Panels:
- Recent errors log stream
- Failed transaction details table
- Recipient lookup times
- Reference script cache stats
- Slow query analysis

## Files to Create

| File | Description |
|------|-------------|
| `cardano/monitoring/dashboards/cardano-overview.json` | Main dashboard |
| `cardano/monitoring/dashboards/cardano-operations.json` | Ops dashboard |
| `cardano/monitoring/dashboards/cardano-debugging.json` | Debug dashboard |
| `cardano/monitoring/README.md` | Documentation |

## Definition of Done

- [ ] All three dashboards created as JSON files
- [ ] Dashboards importable to Grafana
- [ ] All panels functional with real data
- [ ] Documentation for usage

## Acceptance Criteria

1. Dashboards display useful operational data
2. Easy to import and configure
3. Consistent styling with existing Hyperlane dashboards
