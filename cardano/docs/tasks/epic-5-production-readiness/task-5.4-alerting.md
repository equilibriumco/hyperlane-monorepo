[← Epic 5: Production Readiness](./EPIC.md) | [Epics Overview](../README.md)

# Task 5.4: Alerting Rules
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** [Task 5.2](./task-5.2-prometheus-metrics.md)

## Objective

Define alerting rules for Cardano operations.

## Alert Categories

### Critical Alerts

**Message Processing Stopped**
- Condition: No messages processed in 15 minutes
- Severity: Critical
- Action: Check relayer logs for errors

**High Transaction Failure Rate**
- Condition: Failure rate > 10% over 5 minutes
- Severity: Critical
- Action: Check Blockfrost status, verify script UTXOs

**Blockfrost Unreachable**
- Condition: API down for 5 minutes
- Severity: Critical
- Action: Check Blockfrost status page, consider rotating provider

### Warning Alerts

**Rate Limit Low**
- Condition: Remaining rate limit < 100 requests
- Severity: Warning
- Action: Consider upgrading API tier or adding caching

**High Processing Latency**
- Condition: p95 latency > 60 seconds
- Severity: Warning
- Action: Check Blockfrost latency, review slow queries

**Mailbox Nonce Stuck**
- Condition: Nonce not increasing for 1 hour with pending messages
- Severity: Warning
- Action: Check for stuck transactions, verify ISM configuration

## File to Create

**File:** `cardano/monitoring/alerts/cardano-alerts.yaml`

Alert rules in Prometheus format with:
- Alert name
- Expression
- Duration (for)
- Severity label
- Annotations with summary and runbook reference

## Definition of Done

- [ ] Alert rules defined in YAML
- [ ] Tested in staging environment
- [ ] Runbook references included

## Acceptance Criteria

1. Critical issues alerted quickly
2. No alert fatigue from false positives
3. Clear remediation steps in annotations
