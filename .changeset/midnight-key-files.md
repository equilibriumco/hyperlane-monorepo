---
'@hyperlane-xyz/midnight-sdk': patch
---

A Midnight deploy writes `owner-state.json` readable by its owner only and
will not start while the file is readable by others, and the private-state
store's password is required off a local devnet.
