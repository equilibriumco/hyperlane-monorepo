import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner } from '@hyperlane-xyz/provider-sdk/altvm';
import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  AnnotatedTx,
  TxReceipt,
} from '@hyperlane-xyz/provider-sdk/module';
import type {
  DeployedRawWarpArtifact,
  DeployedWarpAddress,
  IRawWarpArtifactManager,
  RawWarpArtifactConfigs,
  WarpType,
} from '@hyperlane-xyz/provider-sdk/warp';
import { ZERO_ADDRESS_HEX_32, addressToBytes32 } from '@hyperlane-xyz/utils';

import { bytesToHex, hexToBytes } from '../utils/conversion.js';
import { unsupportedOnMidnight } from '../utils/errors.js';
import type { MidnightTransaction } from '../utils/types.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { readRemoteRouters } from '../clients/state.js';

class MidnightNativeWarpReader implements ArtifactReader<
  RawWarpArtifactConfigs['native'],
  DeployedWarpAddress
> {
  constructor(
    protected readonly client: MidnightReadClient,
    protected readonly metadata: ChainMetadataForAltVM,
  ) {}

  async read(
    address: string,
  ): Promise<
    ArtifactDeployed<RawWarpArtifactConfigs['native'], DeployedWarpAddress>
  > {
    const state = await this.client.requireContractState(address);
    const [owner, localDecimals, messageDecimals] = await Promise.all([
      this.client.runCircuit<Uint8Array>('night', state.data, 'owner'),
      this.client.runCircuit<bigint>('night', state.data, 'localDecimals'),
      this.client.runCircuit<bigint>('night', state.data, 'messageDecimals'),
    ]);
    const remoteRouters: Record<number, { address: string }> = {};
    // Per-destination gas has no on-chain slot on Midnight; it lives in
    // warp config only, so the derived config reports none.
    const destinationGas: Record<number, string> = {};
    for (const { domainId, router } of readRemoteRouters(state.data)) {
      remoteRouters[domainId] = { address: router };
    }
    const self = {
      artifactState: ArtifactState.UNDERIVED,
      deployed: { address },
    } as const;
    const native = this.metadata.nativeToken;
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: {
        type: 'native',
        owner: bytesToHex(owner),
        mailbox: address,
        interchainSecurityModule: self,
        hook: self,
        remoteRouters,
        destinationGas,
        name: native?.name,
        symbol: native?.symbol,
        decimals: Number(localDecimals),
        scale: 10 ** Number(messageDecimals - localDecimals),
      },
      deployed: { address },
    };
  }
}

// The night warp route is the mailbox monolith itself, born in core deploy
// with its ISM/hook/decimals sealed. Mutable surface: remote router
// enrollment and ownership. Per-destination gas has no on-chain slot.
class MidnightNativeWarpWriter
  extends MidnightNativeWarpReader
  implements
    ArtifactWriter<RawWarpArtifactConfigs['native'], DeployedWarpAddress>
{
  async create(): Promise<
    [
      ArtifactDeployed<RawWarpArtifactConfigs['native'], DeployedWarpAddress>,
      TxReceipt[],
    ]
  > {
    throw new Error(
      'the Midnight native warp route is the night monolith, deployed by ' +
        '`hyperlane core deploy` — there is no standalone warp token to create',
    );
  }

  async update(
    artifact: ArtifactDeployed<
      RawWarpArtifactConfigs['native'],
      DeployedWarpAddress
    >,
  ): Promise<MidnightTransaction[]> {
    const expected = artifact.config;
    const address = artifact.deployed.address;
    const current = (await this.read(address)).config;
    const txs: MidnightTransaction[] = [];

    // Sealed constructor values: a mismatch is a config error, not a diff.
    if (
      expected.decimals !== undefined &&
      expected.decimals !== current.decimals
    ) {
      throw new Error(
        `night decimals are sealed at deploy (${current.decimals}); config says ${expected.decimals}`,
      );
    }
    if (expected.scale !== undefined && expected.scale !== current.scale) {
      throw new Error(
        `night scale is sealed at deploy (${current.scale}); config says ${expected.scale}`,
      );
    }

    const currentRouters = normalizeRouters(current.remoteRouters);
    const expectedRouters = normalizeRouters(expected.remoteRouters);

    for (const [domain, router] of expectedRouters) {
      if (currentRouters.get(domain) === router) continue;
      // enrollRemoteRouter overwrites an existing enrollment for the domain.
      txs.push({
        annotation: `Enroll remote router ${router} for domain ${domain}`,
        contract: 'night',
        contractAddress: address,
        circuit: 'enrollRemoteRouter',
        args: [BigInt(domain), hexToBytes(router)],
      });
    }
    for (const [domain] of currentRouters) {
      if (expectedRouters.has(domain)) continue;
      txs.push({
        annotation: `Unenroll remote router for domain ${domain}`,
        contract: 'night',
        contractAddress: address,
        circuit: 'unenrollRemoteRouter',
        args: [BigInt(domain)],
      });
    }

    if (
      expected.owner &&
      expected.owner !== ZERO_ADDRESS_HEX_32 &&
      expected.owner.toLowerCase() !== current.owner.toLowerCase()
    ) {
      const ownerBytes = hexToBytes(expected.owner);
      if (ownerBytes.length !== 32) {
        throw new Error(
          `night owner must be a 32-byte ZOwnablePK commitment, got ${expected.owner}`,
        );
      }
      txs.push({
        annotation: `Transfer night ownership to ${expected.owner}`,
        contract: 'night',
        contractAddress: address,
        circuit: 'transferOwnership',
        args: [ownerBytes],
      });
    }

    return txs;
  }
}

// Routers arrive as 20-byte eth addresses or 32-byte hex depending on the
// counterparty chain; the contract stores Bytes<32>.
function normalizeRouters(
  routers: Record<number | string, { address: string }> | undefined,
): Map<number, string> {
  const normalized = new Map<number, string>();
  for (const [domain, { address }] of Object.entries(routers ?? {})) {
    normalized.set(Number(domain), addressToBytes32(address).toLowerCase());
  }
  return normalized;
}

export class MidnightWarpArtifactManager implements IRawWarpArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(private readonly chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readWarpToken(address: string): Promise<DeployedRawWarpArtifact> {
    return this.createReader('native').read(address);
  }

  supportsHookUpdates(): boolean {
    return false;
  }

  createReader<T extends WarpType>(
    type: T,
  ): ArtifactReader<RawWarpArtifactConfigs[T], DeployedWarpAddress> {
    const readers: {
      [K in WarpType]: () => ArtifactReader<
        RawWarpArtifactConfigs[K],
        DeployedWarpAddress
      >;
    } = {
      native: () =>
        new MidnightNativeWarpReader(this.client, this.chainMetadata),
      collateral: unsupportedOnMidnight('warp token', 'collateral'),
      synthetic: unsupportedOnMidnight('warp token', 'synthetic'),
      crossCollateral: unsupportedOnMidnight('warp token', 'crossCollateral'),
    };
    return readers[type]();
  }

  createWriter<T extends WarpType>(
    type: T,
    _signer: ISigner<AnnotatedTx, TxReceipt>,
  ): ArtifactWriter<RawWarpArtifactConfigs[T], DeployedWarpAddress> {
    const writers: {
      [K in WarpType]: () => ArtifactWriter<
        RawWarpArtifactConfigs[K],
        DeployedWarpAddress
      >;
    } = {
      native: () =>
        new MidnightNativeWarpWriter(this.client, this.chainMetadata),
      collateral: unsupportedOnMidnight('warp token', 'collateral'),
      synthetic: unsupportedOnMidnight('warp token', 'synthetic'),
      crossCollateral: unsupportedOnMidnight('warp token', 'crossCollateral'),
    };
    return writers[type]();
  }
}
