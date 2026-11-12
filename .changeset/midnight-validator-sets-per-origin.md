---
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/cli': patch
---

The Midnight night contract now holds one validator set per origin chain, and
the SDK reads and writes its ISM as a `domainRoutingIsm` with one
`messageIdMultisigIsm` per origin chain. `core deploy` passes the lowest
origin's validators to the constructor, which takes the origin as a new
argument, and sets every other origin's validators through the owner.
`core apply` replaces the set of an origin that changed, sets a new origin and
removes an origin that is no longer in the config, and `core read` reports one
entry per origin. A config that still writes the ISM as a single multisig is
refused by `core deploy`, `core apply` and `warp apply` instead of applying
nothing. The number of validator slots is read from the compiled contract, the
chunked deploy plan carries the new circuits, and the state readers follow the
new ledger layout. After a deploy the registry records night under
`domainRoutingIsmFactory` instead of `staticMessageIdMultisigIsmFactory`. Other
protocols are not affected.
