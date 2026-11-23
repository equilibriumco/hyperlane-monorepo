---
'@hyperlane-xyz/midnight-sdk': patch
---

A Midnight transfer out or gas payment that the node refuses because another
one changed the contract first is built again on the new state and sent again,
up to four sends in all. The refused one is in no block, so nothing is paid
twice.
