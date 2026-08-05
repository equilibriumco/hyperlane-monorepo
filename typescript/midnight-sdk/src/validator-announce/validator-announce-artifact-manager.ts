import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import {
  ArtifactState,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedRawValidatorAnnounceArtifact,
  DeployedValidatorAnnounceAddress,
  IRawValidatorAnnounceArtifactManager,
  RawValidatorAnnounceArtifactConfigs,
  ValidatorAnnounceType,
} from '@hyperlane-xyz/provider-sdk/validator-announce';

import { MidnightReadClient } from '../clients/read-client.js';
import { readVaMailboxAddress } from '../clients/state.js';

class MidnightValidatorAnnounceReader implements ArtifactReader<
  RawValidatorAnnounceArtifactConfigs['validatorAnnounce'],
  DeployedValidatorAnnounceAddress
> {
  constructor(private readonly client: MidnightReadClient) {}

  async read(address: string): Promise<DeployedRawValidatorAnnounceArtifact> {
    const state = await this.client.requireContractState(address);
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: { mailboxAddress: readVaMailboxAddress(state.data) },
      deployed: { address },
    };
  }
}

export class MidnightValidatorAnnounceArtifactManager implements IRawValidatorAnnounceArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readValidatorAnnounce(
    address: string,
  ): Promise<DeployedRawValidatorAnnounceArtifact> {
    return this.createReader('validatorAnnounce').read(address);
  }

  createReader<T extends ValidatorAnnounceType>(
    _type: T,
  ): ArtifactReader<
    RawValidatorAnnounceArtifactConfigs[T],
    DeployedValidatorAnnounceAddress
  > {
    return new MidnightValidatorAnnounceReader(this.client);
  }

  createWriter<T extends ValidatorAnnounceType>(
    _type: T,
  ): ArtifactWriter<
    RawValidatorAnnounceArtifactConfigs[T],
    DeployedValidatorAnnounceAddress
  > {
    throw new Error(
      'MidnightValidatorAnnounceArtifactManager.createWriter: not implemented yet (#105)',
    );
  }
}
