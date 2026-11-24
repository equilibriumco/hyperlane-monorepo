---
'@hyperlane-xyz/cli': patch
'@hyperlane-xyz/midnight-sdk': patch
---

On a Midnight chain, `core deploy`, `core apply`, `warp apply` and `hook apply`
refuse an owner other than zero or the commitment the chain holds, instead of
sending it as a new owner id. `core deploy` and `hook apply` refuse a zero IGP
gas price or exchange rate before sending anything and print the price a
delivery costs. `warp deploy` and `warp apply` stop when a token of a route
with night turns amounts into other message amounts than night does. The
chunked deploy stops on a maintenance update that did not fully succeed.
Other protocols are not affected.
