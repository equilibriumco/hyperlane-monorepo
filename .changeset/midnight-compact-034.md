---
'@hyperlane-xyz/midnight-sdk': patch
---

Moved the Compact toolchain dependencies to the 0.34.0 line. Bindings emitted
by the 0.34.0 compiler assert `compact-runtime` 0.19.0 on their first line, and
`midnight-js` 5.0.0-beta.6 cannot read a 0.34.0 contract object at all, failing
a core deploy with `Cannot read properties of undefined (reading 'ctor')`.
`compact-runtime` goes to 0.19.0-rc.0, `compact-js` to 2.5.5-rc.8, and the
seven `midnight-js` packages to 5.0.0-beta.7, which is the set pinning the same
runtime, so the tree resolves a single `compact-runtime` copy.
