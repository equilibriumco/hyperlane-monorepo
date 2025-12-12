[← Epic 2: Token Bridge](./EPIC.md) | [Epics Overview](../README.md)

# Task 2.4: Remote Route Enrollment
**Status:** ⬜ Not Started
**Complexity:** Low
**Depends On:** [Task 2.2](./task-2.2-collateral-route.md), [Task 2.3](./task-2.3-synthetic-route.md)

## Objective

Implement remote router enrollment to connect Cardano warp routes with routes on other chains.

## Background

Warp routes need to know their counterpart addresses on other chains. This is done through enrollment, which stores the mapping of domain ID to router address.

## Requirements

### 1. CLI Enroll Command

Implement command to enroll a remote router:
- `--local-route <script_hash>` - The Cardano warp route
- `--remote-domain <domain_id>` - The destination chain domain
- `--remote-router <address>` - The remote warp route address (32-byte hex)

### 2. Query Enrolled Routes

Implement `warp show` command that displays:
- Warp route details (type, token, etc.)
- All enrolled remote routes with their domain and address

### 3. On-Chain Validation

The enrollment transaction must verify:
- Only owner can enroll routes
- Router address is 32 bytes
- Domain is valid

## Files to Modify

| File | Changes |
|------|---------|
| `cardano/cli/src/commands/warp.rs` | Add enroll and show commands |

## Testing

- Enroll command updates state correctly
- Show command displays enrolled routes
- Only owner can enroll (access control)
- Invalid router addresses rejected

## Definition of Done

- [ ] Enroll command implemented
- [ ] Show command displays routes
- [ ] Works on testnet
- [ ] Ready for transfer testing

## Acceptance Criteria

1. Remote routes correctly stored in warp route state
2. Query shows all enrolled routes
3. Access control enforced (owner-only)
