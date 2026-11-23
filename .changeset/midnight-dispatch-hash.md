---
'@hyperlane-xyz/midnight-sdk': patch
---

The Midnight night contract no longer stores the messages it dispatches. It
keeps a count and a running hash over their ids, read through `dispatchHash`,
which takes `messageAt`'s place in the chunked deploy's circuit list. Other
protocols are not affected.
