---
"@hyperlane-xyz/midnight-sdk": patch
---

A Midnight deploy now refuses a contract module whose compiler-recorded key fingerprints are not the verifier keys being deployed, verifies every circuit on chain for every contract in both deploy modes and names each missing, extra or wrong key, and the artifact copy is cleared before it is refreshed.
