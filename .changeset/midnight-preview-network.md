---
'@hyperlane-xyz/cli': patch
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/sdk': patch
---

The note a Midnight deploy prints points at the contracts repo's deployments
folder for the known address list, and the Midnight docs and examples name
preview as the public network. The private-state password is checked against
Midnight.js's rules when the store is opened, so a weak one stops a deploy
before anything is sent.
