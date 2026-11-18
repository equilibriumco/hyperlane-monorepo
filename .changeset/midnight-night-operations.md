---
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/cli': patch
---

`hyperlane warp midnight` funds, pauses and unpauses the night contract of a
Midnight route, and `status` shows whether it is paused and what its escrow
holds. `core apply` on a Midnight chain refuses an IGP block in
`requiredHook`, which deployed a new IGP that nothing used. A turbo build of
the Midnight SDK now sees `HYPERLANE_MIDNIGHT_CONTRACTS` and copies the
contracts from it on every build, since turbo cannot tell when they change.
