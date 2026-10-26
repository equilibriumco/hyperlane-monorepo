---
'@hyperlane-xyz/cli': patch
---

The CLI's default registry is now a pinned commit of the Midnight registry
instead of a moving branch; `--registry` still overrides it. With
`HYPERLANE_KNOWN_ADDRESSES` set to a known-good address list, every command
but the deploys stops with `registry address check failed: ...` when the
registry's addresses for the two chains the list names match none of its
deployment entries, and warns once when the variable is unset. A Midnight
`core deploy` or `warp deploy` ends by asking for the new addresses to be
added to that list from the deploy record.
