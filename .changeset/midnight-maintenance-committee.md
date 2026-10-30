---
'@hyperlane-xyz/midnight-sdk': patch
---

Every Midnight contract deploy now ends by handing the contract's maintenance
authority to a committee with a threshold of two, so no single key can replace
a verifier key or the authority. `MIDNIGHT_MAINTENANCE_COMMITTEE` names the
holders' verifying keys (comma-separated `<kind>:<hex>`, at least three) and
the deploy host then keeps no key; unset, the committee is the deploy key plus
two sampled keys, saved together in `owner-state.json`. The deploy reads the
authority back and fails if it is not the committee it set.
