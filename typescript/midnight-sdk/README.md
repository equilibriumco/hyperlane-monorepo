# Hyperlane Midnight SDK

The Hyperlane Midnight SDK is a fully typed TypeScript SDK for the [Midnight Implementation](https://github.com/equilibriumco/hyperlane-midnight).
It can be used as a standalone SDK for frontend or in backend applications which want to connect to a Midnight chain which has the Hyperlane contracts deployed.

## Install

```bash
# Install with NPM
npm install @hyperlane-xyz/midnight-sdk

# Or with pnpm
pnpm add @hyperlane-xyz/midnight-sdk
```

## Usage

```ts
import { MidnightProvider, MidnightSigner } from '@hyperlane-xyz/midnight-sdk';
import {
  ChainMetadataForAltVM,
  ProtocolType,
} from '@hyperlane-xyz/provider-sdk';

const metadata: ChainMetadataForAltVM = {
  name: 'midnight',
  protocol: ProtocolType.Midnight,
  chainId: 1234,
  domainId: 1234,
  // rpcUrls point at the node: this is the submit path.
  rpcUrls: [{ http: 'http://localhost:9944' }],
  // gatewayUrls carry the indexer GraphQL endpoint: this is the read path for
  // all chain state. Both are required.
  gatewayUrls: [{ http: 'http://localhost:8088/api/v3/graphql' }],
  // Required. Signing throws without it.
  midnightNetworkId: 'undeployed',
};

const signer = await MidnightSigner.connectWithSigner(metadata, SEED);

const mailbox = await signer.getMailbox({ mailboxAddress });

// performing queries without signer
const provider = await MidnightProvider.connect(metadata);

const mailbox = await provider.getMailbox({ mailboxAddress });
```

Two things differ from the other AltVM SDKs and are worth knowing first:

- **State reads go through the indexer, not the node.** `gatewayUrls` is not
  optional; a chain entry carrying only `rpcUrls` cannot answer a read.
- **`warp deploy` does not create the Midnight side of a route.** Midnight has
  no cross-contract calls, so the token logic lives inside the core contract
  and the route is created by `core deploy`. A warp deploy targets the
  counterpart chain and references the Midnight end as a `foreignDeployment`.

## Environment variables

Submitting a transaction on Midnight means generating a zero-knowledge proof
locally, so this SDK needs a proof server and the compiled contract artifacts
on disk.

- **HYPERLANE_MIDNIGHT_CONTRACTS=\<path>**: the compiled contract tree from
  [hyperlane-midnight](https://github.com/equilibriumco/hyperlane-midnight)
  (`contracts/src/managed`). Export it before **building** this package, not
  only before using it: the build copies the artifacts in, and without it
  copies from whatever sibling checkout it happens to find. The packaged
  fallback carries verifier keys only, so proving fails at run time rather than
  at build time.
- **MIDNIGHT_PROOF_SERVER_URL=\<url>**: proof server to prove against, default
  local. There is no public proof server.
- **MIDNIGHT_NETWORK=devnet/stagenet**: selects default endpoints. Unset on a
  stagenet host means every endpoint quietly points at localhost.
- **MIDNIGHT_STATE_DIR=\<path>**: holds the ownership secret and private state.
  Losing it forfeits contract ownership and the ability to update circuits, so
  back it up after a deploy.
- **MIDNIGHT_STATE_PASSWORD=\<password>**: encrypts that state. There is no
  rotation path, so set it before first use.
- **MIDNIGHT_SUBMIT_TIMEOUT_SECS=\<seconds>**: submit timeout. The default is
  well below the time an inbound delivery proof takes, so raise it when running
  agents.

## Setup

Node 18 or newer is required.

Building this package requires the compiled Compact artifacts described above;
compile them in the contracts repository first.
