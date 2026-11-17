---
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/sdk': patch
'@hyperlane-xyz/cli': patch
---

A Midnight core deploy writes the chain's height from before the deploy into
the registry chain metadata as `index.from`, and a warp deploy fills the
Midnight token's `igpTokenAddressOrDenom` from the chain's IGP in the
registry, so neither is added by hand. The generated agent config no longer
carries `indexerGraphqlUrl`: the agents read the indexer from `gatewayUrls`.
The chunked night deploy's key budget per transaction is fixed.
