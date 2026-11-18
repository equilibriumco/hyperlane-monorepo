---
'@hyperlane-xyz/midnight-sdk': patch
---

The Midnight night contract stores each validator's public key as its curve
coordinates instead of 64 bytes. The SDK converts the configured keys when it
writes a validator set and back when it reads one, so configs keep the 64-byte
form. Other protocols are not affected.
