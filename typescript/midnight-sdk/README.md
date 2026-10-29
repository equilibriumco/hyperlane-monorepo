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

## Core config

`core deploy` passes the multisig ISM (`validators`, `validatorPubkeys`,
`threshold`), the chain's `nativeToken.decimals` and one Midnight-only field to
the night contract's constructor. The decimals are fixed for good; `core apply`
changes the validator set and the field below. See
`typescript/cli/examples/midnight/core-config.yaml` for a full example.

- **maxTransferAmount**: per-transfer limit in local NIGHT units, as a decimal
  string or number. `0` or unset at deploy means no limit. With a different
  value, `core apply` changes it through night's owner-only setter; left out
  of the config, the deployed limit stays as it is. `core read` reports it as
  a decimal string.

## Warp send recipients

`warp send` checks the recipient against the destination before anything is
sent. A transfer to a recipient of the wrong kind is NIGHT nobody receives:
night pays an inbound transfer to the recipient bytes as a user address, the
EVM router mints an outbound one to the last 20 bytes, and neither refunds.

- **To Midnight**: give the recipient in the wallet's unshielded address form
  (`mn_addr...`, `mn_addr_undeployed...` on a local devnet), which names a user
  address and nothing else. Raw 32-byte hex is refused, and so is an address
  for another network than the destination's `midnightNetworkId`. Contract
  addresses in registries and configs stay hex.
- **From Midnight**: an EVM destination takes a 20-byte address, or its 32-byte
  zero-padded form through the SDK. The zero address, and 32 bytes whose first
  12 are not zero, are refused.

The SDK applies the same rules in `WarpCore.validateRecipient` and in the
Midnight token adapter, so a transfer built without the CLI gets them too, and
the transaction builder refuses an all-zero recipient on every route.

## Registry

The CLI's default registry is upstream's, which has no Midnight chain, so pass
`--registry` with a checkout of
[equilibriumco/hyperlane-registry](https://github.com/equilibriumco/hyperlane-registry)
at the commit the deployment guide pins. Move the pin when the registry's
`midnight` branch changes.

The addresses a registry serves can also be checked against a known-good list,
the contracts repo's `deployments/stagenet-sepolia.json`. Only registries and
configs with a Midnight chain are checked, so nothing changes for other chains:

- **HYPERLANE_KNOWN_ADDRESSES=\<path>**: read by the CLI on every command
  except the deploys, once a registry that serves a Midnight chain is loaded.
  The core addresses of the two chains the file names, the route's token on
  each and the remote token's ISM must match one deployment entry (a `null`
  field in the entry is skipped), or the command stops with
  `registry address check failed: ...`. Unset, the command warns once that the
  addresses were not checked.
- **knownAddresses** (`HYP_KNOWNADDRESSES=\<path>`): the same check in the
  relayer and the validator at startup, on the core addresses of those chains
  in the config the agent will actually use. Unset, an agent with a Midnight
  chain warns that the addresses were not checked. The scraper does not check.

`core deploy` and `warp deploy` create new addresses, so they do not check;
after a Midnight deploy, add the addresses to the known list from the deploy
record, not from the registry.

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
  back it up after a deploy. Its `owner-state.json` is written readable by your
  user only, and a deploy will not start while it is readable by others.
- **MIDNIGHT_STATE_PASSWORD=\<password>**: encrypts that state, and is required
  off a local devnet. There is no rotation path, so set it before first use.
- **MIDNIGHT_SUBMIT_TIMEOUT_SECS=\<seconds>**: submit timeout. The default is
  well below the time an inbound delivery proof takes, so raise it when running
  agents.

## Setup

Node 18 or newer is required.

Building this package requires the compiled Compact artifacts described above;
compile them in the contracts repository first.
