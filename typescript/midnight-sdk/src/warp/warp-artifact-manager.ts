import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedRawWarpArtifact,
  DeployedWarpAddress,
  IRawWarpArtifactManager,
  RawWarpArtifactConfigs,
  WarpType,
} from '@hyperlane-xyz/provider-sdk/warp';

import { bytesToHex } from '../utils/conversion.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { readRemoteRouters } from '../clients/state.js';

class MidnightNativeWarpReader implements ArtifactReader<
  RawWarpArtifactConfigs['native'],
  DeployedWarpAddress
> {
  constructor(
    private readonly client: MidnightReadClient,
    private readonly metadata: ChainMetadataForAltVM,
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

export class MidnightWarpArtifactManager implements IRawWarpArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(private readonly chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readWarpToken(address: string): Promise<DeployedRawWarpArtifact> {
    return this.createReader('native').read(
      address,
    ) as Promise<DeployedRawWarpArtifact>;
  }

  supportsHookUpdates(): boolean {
    return false;
  }

  createReader<T extends WarpType>(
    type: T,
  ): ArtifactReader<RawWarpArtifactConfigs[T], DeployedWarpAddress> {
    if (type !== 'native') {
      throw new Error(
        `unsupported warp token type on Midnight: ${type}, only native NIGHT exists`,
      );
    }
    return new MidnightNativeWarpReader(
      this.client,
      this.chainMetadata,
    ) as unknown as ArtifactReader<
      RawWarpArtifactConfigs[T],
      DeployedWarpAddress
    >;
  }

  createWriter<T extends WarpType>(
    _type: T,
  ): ArtifactWriter<RawWarpArtifactConfigs[T], DeployedWarpAddress> {
    throw new Error(
      'MidnightWarpArtifactManager.createWriter: not implemented yet (#105)',
    );
  }
}
