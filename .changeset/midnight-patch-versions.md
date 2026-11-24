---
'@hyperlane-xyz/midnight-sdk': patch
---

Moved the Midnight dependencies to the patch releases the public networks now
list: `midnight-js` 5.0.1, `compact-js` 3.0.2 and the wallet SDK 2.0.1. They
pin `ledger-v9` 1.0.2 and `onchain-runtime-v4` 4.0.2 themselves, so the
workspace no longer forces either. Other protocols are not affected.
