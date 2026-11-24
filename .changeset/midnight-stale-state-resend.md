---
'@hyperlane-xyz/midnight-sdk': patch
---

A Midnight transfer out or gas payment that the node refuses on submit (error
104) because another one changed the contract first is built again on the new
state and sent again, up to four sends in all. A call refused on submit never
left the node, so nothing is paid twice. One the node drops from its pool as
invalid is not sent again, since a fork can still put it in a block.
