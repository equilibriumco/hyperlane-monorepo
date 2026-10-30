---
'@hyperlane-xyz/utils': patch
'@hyperlane-xyz/sdk': patch
'@hyperlane-xyz/midnight-sdk': patch
'@hyperlane-xyz/cli': patch
---

A warp transfer to Midnight takes its recipient in the unshielded address form
(`mn_addr...`), which the utils decode to the 32 bytes on the wire when the
protocol is named as Midnight; raw hex is refused, and so is an address for
another network than the destination's `midnightNetworkId`, when the chain
metadata names one. A transfer from Midnight to an EVM chain refuses a
recipient the destination cannot mint to: the zero address, or 32 bytes whose
first 12 are not zero. The CLI now checks the recipient for a Midnight origin
too, where it used to skip all validation because of the balance step. Other
chains are not affected.
