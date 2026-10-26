---
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/provider-sdk': patch
'@hyperlane-xyz/sdk': patch
'@hyperlane-xyz/cli': patch
---

A Midnight core deploy now passes the night contract's ninth constructor
argument, `maxTransferAmount`: a per-transfer limit in local NIGHT units read
from the optional core config field of the same name (a decimal string or
number), 0 or unset meaning no limit. The field travels from the core config
schema through the provider-sdk mailbox config to the night writer.
