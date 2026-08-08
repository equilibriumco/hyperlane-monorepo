# Hyperlane Midnight SDK

Implements the `ProtocolProvider` interface from `@hyperlane-xyz/provider-sdk`
for the Midnight network, making Midnight a first-class protocol in the
Hyperlane CLI (`core`/`warp` `read`, `check`, `deploy`, `apply`, `send`).

Midnight chain metadata carries two endpoint sets: `rpcUrls` point at the
node, while `gatewayUrls` carry the indexer GraphQL endpoint, which is the
read path for all chain state.

Status: under construction. The provider read surface, signer, and artifact
managers are being implemented incrementally.
