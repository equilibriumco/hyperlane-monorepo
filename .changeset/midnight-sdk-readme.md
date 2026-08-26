---
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/cli': patch
---

Documented the Midnight SDK to match the other AltVM SDK packages: install, a
usage example, and the environment variables it reads. Records the two things
that differ from its siblings — chain state is read through the indexer via
`gatewayUrls` rather than from the node, and `warp deploy` does not create the
Midnight side of a route because the token logic lives inside the core
contract. Adds `midnight` to the CLI's supported-protocol lists, which omitted
a protocol the code already accepts.
