---
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/provider-sdk': patch
'@hyperlane-xyz/deploy-sdk': patch
'@hyperlane-xyz/sdk': patch
'@hyperlane-xyz/cli': patch
---

A Midnight core deploy now passes the night contract's ninth constructor
argument, `maxTransferAmount`: a per-transfer limit in local NIGHT units read
from the optional core config field of the same name (a decimal string or
number), 0 or unset meaning no limit. The chunked deploy plan carries the two
new circuits. `core read` reports the limit as a decimal string, `core check`
compares it when the config sets it, and `core apply` changes it through
night's owner-only setter. The field passes through the shared core writer,
reader and derived config only when it is set, so other protocols see the
same configs as before.
