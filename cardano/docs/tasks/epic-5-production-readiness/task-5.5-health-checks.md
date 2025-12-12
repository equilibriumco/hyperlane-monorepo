[← Epic 5: Production Readiness](./EPIC.md) | [Epics Overview](../README.md)

# Task 5.5: Health Check Endpoint
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** None

## Objective

Implement health check endpoint for Kubernetes probes and monitoring.

## Requirements

### Health Endpoint

`GET /health`

Should return JSON with:
- Overall status (healthy/degraded/unhealthy)
- Timestamp
- Component statuses:
  - Blockfrost: status, latency, rate limit remaining
  - Mailbox: status, inbound/outbound nonce, last checked
  - Registry: status, recipient count
- Last block info: height, slot, time

### Status Values

- `healthy`: All checks pass
- `degraded`: Non-critical issues (e.g., high latency)
- `unhealthy`: Critical issues (e.g., Blockfrost unreachable)

### Component Checks

**Blockfrost Check:**
- Make a simple API call (e.g., get latest block)
- Measure latency
- Check rate limit remaining

**Mailbox Check:**
- Verify mailbox UTXO is accessible
- Return current nonces

**Registry Check:**
- Verify registry UTXO is accessible
- Return recipient count

## Kubernetes Integration

The endpoint should be usable for:
- Liveness probe: Is the service running?
- Readiness probe: Is the service ready to accept traffic?

Typical configuration:
- Liveness: Check every 30s, fail after 3 attempts
- Readiness: Check every 10s, fail after 1 attempt

## Files to Create/Modify

| File | Changes |
|------|---------|
| `rust/main/chains/hyperlane-cardano/src/health.rs` | Health check logic |
| Server setup | Add /health route |

## Testing

- Returns healthy when all OK
- Returns unhealthy on Blockfrost failure
- Response time acceptable (<500ms)
- Works with k8s probes

## Definition of Done

- [ ] Health endpoint implemented
- [ ] All components checked
- [ ] Works with k8s probes
- [ ] Documented

## Acceptance Criteria

1. Accurate health status reported
2. Fast response time (<500ms)
3. Useful diagnostic information included
