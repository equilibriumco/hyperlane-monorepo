import { keccak_256 } from '@noble/hashes/sha3.js';

import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedIsmAddress,
  DeployedRawIsmArtifact,
  IRawIsmArtifactManager,
  IsmType,
  RawIsmArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/ism';

import { bytesToHex } from '../utils/conversion.js';
import { unsupportedOnMidnight } from '../utils/errors.js';
import { MidnightReadClient } from '../clients/read-client.js';

const MODULE_TYPE_MESSAGE_ID_MULTISIG = 5n;

// Validators are enrolled as 64-byte secp256k1 pubkeys (X || Y); the
// Hyperlane validator identity is the derived Ethereum address.
function pubkeyToEthAddress(pubkey: Uint8Array): string {
  if (pubkey.length !== 64) {
    throw new Error(
      `expected a 64-byte validator pubkey, got ${pubkey.length} bytes`,
    );
  }
  return bytesToHex(keccak_256(pubkey).slice(-20));
}

class MidnightMultisigIsmReader implements ArtifactReader<
  RawIsmArtifactConfigs['messageIdMultisigIsm'],
  DeployedIsmAddress
> {
  constructor(private readonly client: MidnightReadClient) {}

  async read(
    address: string,
  ): Promise<
    ArtifactDeployed<
      RawIsmArtifactConfigs['messageIdMultisigIsm'],
      DeployedIsmAddress
    >
  > {
    const state = await this.client.requireContractState(address);
    const [count, threshold] = await Promise.all([
      this.client.runCircuit<bigint>('night', state.data, 'validatorCount'),
      this.client.runCircuit<bigint>('night', state.data, 'thresholdValue'),
    ]);
    const validators: string[] = [];
    for (let i = 0n; i < count; i++) {
      const pubkey = await this.client.runCircuit<Uint8Array>(
        'night',
        state.data,
        'validatorAt',
        [i],
      );
      validators.push(pubkeyToEthAddress(pubkey));
    }
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: {
        type: 'messageIdMultisigIsm',
        validators,
        threshold: Number(threshold),
      },
      deployed: { address },
    };
  }
}

export class MidnightIsmArtifactManager implements IRawIsmArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readIsm(address: string): Promise<DeployedRawIsmArtifact> {
    const state = await this.client.requireContractState(address);
    const moduleType = await this.client.runCircuit<bigint>(
      'night',
      state.data,
      'moduleType',
    );
    if (moduleType !== MODULE_TYPE_MESSAGE_ID_MULTISIG) {
      throw new Error(
        `unexpected ISM module type ${moduleType} at ${address}, only messageIdMultisigIsm (5) exists on Midnight`,
      );
    }
    return this.createReader('messageIdMultisigIsm').read(address);
  }

  createReader<T extends IsmType>(
    type: T,
  ): ArtifactReader<RawIsmArtifactConfigs[T], DeployedIsmAddress> {
    const readers: {
      [K in IsmType]: () => ArtifactReader<
        RawIsmArtifactConfigs[K],
        DeployedIsmAddress
      >;
    } = {
      messageIdMultisigIsm: () => new MidnightMultisigIsmReader(this.client),
      merkleRootMultisigIsm: unsupportedOnMidnight(
        'ISM',
        'merkleRootMultisigIsm',
      ),
      domainRoutingIsm: unsupportedOnMidnight('ISM', 'domainRoutingIsm'),
      testIsm: unsupportedOnMidnight('ISM', 'testIsm'),
      compositeIsm: unsupportedOnMidnight('ISM', 'compositeIsm'),
    };
    return readers[type]();
  }

  createWriter<T extends IsmType>(
    _type: T,
  ): ArtifactWriter<RawIsmArtifactConfigs[T], DeployedIsmAddress> {
    throw new Error(
      'MidnightIsmArtifactManager.createWriter: not implemented yet (#105)',
    );
  }
}
