import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedHookAddress,
  DeployedHookArtifact,
  HookType,
  IRawHookArtifactManager,
  RawHookArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/hook';

import { bytesToHex } from '../utils/conversion.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { readRemoteGasData, topLevelArity } from '../clients/state.js';

const NIGHT_STATE_ARITY = 2;
const IGP_STATE_ARITY = 8;

type EitherAddress = {
  is_left: boolean;
  left: { bytes: Uint8Array };
  right: { bytes: Uint8Array };
};

class MidnightMerkleTreeHookReader implements ArtifactReader<
  RawHookArtifactConfigs['merkleTreeHook'],
  DeployedHookAddress
> {
  async read(
    address: string,
  ): Promise<
    ArtifactDeployed<
      RawHookArtifactConfigs['merkleTreeHook'],
      DeployedHookAddress
    >
  > {
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: { type: 'merkleTreeHook' },
      deployed: { address },
    };
  }
}

class MidnightIgpHookReader implements ArtifactReader<
  RawHookArtifactConfigs['interchainGasPaymaster'],
  DeployedHookAddress
> {
  constructor(private readonly client: MidnightReadClient) {}

  async read(
    address: string,
  ): Promise<
    ArtifactDeployed<
      RawHookArtifactConfigs['interchainGasPaymaster'],
      DeployedHookAddress
    >
  > {
    const state = await this.client.requireContractState(address);
    const [owner, beneficiary] = await Promise.all([
      this.client.runCircuit<Uint8Array>('igp', state.data, 'owner'),
      this.client.runCircuit<EitherAddress>(
        'igp',
        state.data,
        'beneficiaryValue',
      ),
    ]);
    const oracleConfig: Record<
      number,
      { gasPrice: string; tokenExchangeRate: string }
    > = {};
    const overhead: Record<number, number> = {};
    for (const entry of readRemoteGasData(state.data)) {
      oracleConfig[entry.domainId] = {
        gasPrice: entry.gasPrice,
        tokenExchangeRate: entry.tokenExchangeRate,
      };
      overhead[entry.domainId] = 0;
    }
    const ownerHex = bytesToHex(owner);
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: {
        type: 'interchainGasPaymaster',
        owner: ownerHex,
        beneficiary: bytesToHex(
          beneficiary.is_left
            ? beneficiary.left.bytes
            : beneficiary.right.bytes,
        ),
        oracleKey: ownerHex,
        overhead,
        oracleConfig,
      },
      deployed: { address },
    };
  }
}

export class MidnightHookArtifactManager implements IRawHookArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readHook(address: string): Promise<DeployedHookArtifact> {
    const state = await this.client.requireContractState(address);
    const arity = topLevelArity(state.data);
    switch (arity) {
      case NIGHT_STATE_ARITY:
        return this.createReader('merkleTreeHook').read(
          address,
        ) as Promise<DeployedHookArtifact>;
      case IGP_STATE_ARITY:
        return this.createReader('interchainGasPaymaster').read(
          address,
        ) as Promise<DeployedHookArtifact>;
      default:
        throw new Error(
          `contract at ${address} is neither the night monolith nor the igp (top-level state arity ${arity})`,
        );
    }
  }

  createReader<T extends HookType>(
    type: T,
  ): ArtifactReader<RawHookArtifactConfigs[T], DeployedHookAddress> {
    switch (type) {
      case 'merkleTreeHook':
        return new MidnightMerkleTreeHookReader() as unknown as ArtifactReader<
          RawHookArtifactConfigs[T],
          DeployedHookAddress
        >;
      case 'interchainGasPaymaster':
        return new MidnightIgpHookReader(
          this.client,
        ) as unknown as ArtifactReader<
          RawHookArtifactConfigs[T],
          DeployedHookAddress
        >;
      default:
        throw new Error(`unsupported hook type on Midnight: ${type}`);
    }
  }

  createWriter<T extends HookType>(
    _type: T,
  ): ArtifactWriter<RawHookArtifactConfigs[T], DeployedHookAddress> {
    throw new Error(
      'MidnightHookArtifactManager.createWriter: not implemented yet (#105)',
    );
  }
}
