---
'@hyperlane-xyz/midnight-sdk': patch
---

Moved the Midnight dependencies to the released versions the public networks
run: `midnight-js` 5.0.0, `compact-js` 3.0.0, `compact-runtime` 0.21.0 and the
wallet SDK 2.0.0, with `ledger-v9` 1.0.2-rc.0 forced to a single copy. The
contracts compiled with Compact 0.36.0 give night four validator slots per
origin. Node 22.12 or newer is required. Other protocols are not affected.
