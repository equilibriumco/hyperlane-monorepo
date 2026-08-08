import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner } from '@hyperlane-xyz/provider-sdk/altvm';
import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactNew,
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
import type {
  AnnotatedTx,
  TxReceipt,
} from '@hyperlane-xyz/provider-sdk/module';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { bytesToHex } from '../utils/conversion.js';
import { unsupportedOnMidnight } from '../utils/errors.js';
import type { MidnightTransaction } from '../utils/types.js';
import { MidnightReadClient } from '../clients/read-client.js';

import {
  pubkeyToEthAddress,
  resolveValidatorSet,
  sameValidatorSet,
} from './validators.js';

const MODULE_TYPE_MESSAGE_ID_MULTISIG = 5n;

class MidnightMultisigIsmReader implements ArtifactReader<
  RawIsmArtifactConfigs['messageIdMultisigIsm'],
  DeployedIsmAddress
> {
  constructor(protected readonly client: MidnightReadClient) {}

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
    // The chain stores full pubkeys; expose both them and the derived eth
    // addresses so a derived config round-trips into write-ready configs
    // (writes need the pubkeys, checks compare the addresses).
    const validators: string[] = [];
    const validatorPubkeys: string[] = [];
    for (let i = 0n; i < count; i++) {
      const pubkey = await this.client.runCircuit<Uint8Array>(
        'night',
        state.data,
        'validatorAt',
        [i],
      );
      validators.push(pubkeyToEthAddress(pubkey));
      validatorPubkeys.push(bytesToHex(pubkey));
    }
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: {
        type: 'messageIdMultisigIsm',
        validators,
        threshold: Number(threshold),
        validatorPubkeys,
      },
      deployed: { address },
    };
  }
}

class MidnightMultisigIsmWriter
  extends MidnightMultisigIsmReader
  implements
    ArtifactWriter<
      RawIsmArtifactConfigs['messageIdMultisigIsm'],
      DeployedIsmAddress
    >
{
  // The ISM is a facet of the night monolith and cannot deploy standalone:
  // validators/threshold are night constructor args, and the contract does
  // not exist yet when the core deploy orchestrator asks for the ISM. The
  // returned zero-address sentinel keeps the config flowing to the mailbox
  // writer, which seals it into the constructor and reports the real
  // (night) address on its own artifact.
  async create(
    artifact: ArtifactNew<RawIsmArtifactConfigs['messageIdMultisigIsm']>,
  ): Promise<
    [
      ArtifactDeployed<
        RawIsmArtifactConfigs['messageIdMultisigIsm'],
        DeployedIsmAddress
      >,
      TxReceipt[],
    ]
  > {
    resolveValidatorSet(artifact.config);
    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: artifact.config,
        deployed: { address: ZERO_ADDRESS_HEX_32 },
      },
      [],
    ];
  }

  async update(
    artifact: ArtifactDeployed<
      RawIsmArtifactConfigs['messageIdMultisigIsm'],
      DeployedIsmAddress
    >,
  ): Promise<MidnightTransaction[]> {
    const expected = artifact.config;
    const current = await this.read(artifact.deployed.address);
    if (sameValidatorSet(current.config, expected)) {
      return [];
    }
    const set = resolveValidatorSet(expected);
    return [
      {
        annotation:
          `Set validators (${expected.validators.length}) and threshold ` +
          `${expected.threshold} on ${artifact.deployed.address}`,
        contract: 'night',
        contractAddress: artifact.deployed.address,
        circuit: 'setValidatorsAndThreshold',
        args: [set.paddedPubkeys, set.count, set.threshold],
      },
    ];
  }
}

export class MidnightIsmArtifactManager implements IRawIsmArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  // setValidatorsAndThreshold rotates the multisig in place; a config
  // change must not force the EVM-style "redeploy the static ISM" path.
  supportsInPlaceStaticIsmUpdates(): boolean {
    return true;
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
    type: T,
    _signer: ISigner<AnnotatedTx, TxReceipt>,
  ): ArtifactWriter<RawIsmArtifactConfigs[T], DeployedIsmAddress> {
    const writers: {
      [K in IsmType]: () => ArtifactWriter<
        RawIsmArtifactConfigs[K],
        DeployedIsmAddress
      >;
    } = {
      messageIdMultisigIsm: () => new MidnightMultisigIsmWriter(this.client),
      merkleRootMultisigIsm: unsupportedOnMidnight(
        'ISM',
        'merkleRootMultisigIsm',
      ),
      domainRoutingIsm: unsupportedOnMidnight('ISM', 'domainRoutingIsm'),
      testIsm: unsupportedOnMidnight('ISM', 'testIsm'),
      compositeIsm: unsupportedOnMidnight('ISM', 'compositeIsm'),
    };
    return writers[type]();
  }
}
