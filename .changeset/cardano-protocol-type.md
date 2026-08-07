---
'@hyperlane-xyz/provider-sdk': patch
'@hyperlane-xyz/widgets': patch
'@hyperlane-xyz/utils': patch
'@hyperlane-xyz/sdk': patch
---

`ProtocolType.Cardano` was added, along with Cardano address utilities: Shelley bech32 recognition and validation, transaction hash validation, and conversion between bech32 addresses and the 32-byte Hyperlane encoding of their payment credential (`[credentialKind, 0x00, 0x00, 0x00, ...28-byte hash]`). A `SdkSupportedProtocol` type and `isSdkSupportedProtocol` guard were introduced for the protocols that have a client-side TypeScript implementation; the provider, signer, token-standard and wallet-integration maps are now keyed by it, since Cardano is reached only through the Rust agents and surfaces in TypeScript as scraped explorer data. A Cardano protocol logo was added to the widgets library.
