---
'@hyperlane-xyz/cli': patch
---

With `HYPERLANE_KNOWN_ADDRESSES` set to a known-good address list, every
command but the deploys, on a registry that serves a Midnight chain, stops with
`registry address check failed: ...` when the registry's addresses for the two
chains the list names match none of its deployment entries; unset, such a
command warns once. A registry with no Midnight chain is not checked. A
Midnight `core deploy` or `warp deploy` ends by asking for the new addresses to
be added to that list from the deploy record.
