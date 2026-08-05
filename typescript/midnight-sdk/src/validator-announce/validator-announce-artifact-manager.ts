import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner } from '@hyperlane-xyz/provider-sdk/altvm';
import {
  ArtifactState,
  type ArtifactNew,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  AnnotatedTx,
  TxReceipt,
} from '@hyperlane-xyz/provider-sdk/module';
import type {
  DeployedRawValidatorAnnounceArtifact,
  DeployedValidatorAnnounceAddress,
  IRawValidatorAnnounceArtifactManager,
  RawValidatorAnnounceArtifactConfigs,
  ValidatorAnnounceType,
} from '@hyperlane-xyz/provider-sdk/validator-announce';

import { hexToBytes } from '../utils/conversion.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { MidnightSigner, requireMidnightSigner } from '../clients/signer.js';
import { readVaMailboxAddress } from '../clients/state.js';

function decodeLocation(buffer: Uint8Array): string {
  const nul = buffer.indexOf(0);
  return Buffer.from(nul === -1 ? buffer : buffer.slice(0, nul)).toString(
    'utf-8',
  );
}

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

// Seals localDomain + the night (mailbox) address into the announcement
// domain hash at construction; nothing is mutable afterwards.
class MidnightValidatorAnnounceWriter
  extends MidnightValidatorAnnounceReader
  implements
    ArtifactWriter<
      RawValidatorAnnounceArtifactConfigs['validatorAnnounce'],
      DeployedValidatorAnnounceAddress
    >
{
  constructor(
    client: MidnightReadClient,
    private readonly metadata: ChainMetadataForAltVM,
    private readonly signer: MidnightSigner,
  ) {
    super(client);
  }

  async create(
    artifact: ArtifactNew<
      RawValidatorAnnounceArtifactConfigs['validatorAnnounce']
    >,
  ): Promise<[DeployedRawValidatorAnnounceArtifact, TxReceipt[]]> {
    const mailboxBytes = hexToBytes(artifact.config.mailboxAddress);
    if (mailboxBytes.length !== 32) {
      throw new Error(
        `mailbox address must be 32 bytes, got ${artifact.config.mailboxAddress}`,
      );
    }
    const { address, receipts } = await this.signer.deployMidnightContract({
      name: 'validator-announce',
      buildArgs: ({ ownerId, instanceSalt }) => [
        ownerId,
        instanceSalt,
        BigInt(this.metadata.domainId),
        mailboxBytes,
      ],
    });
    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: artifact.config,
        deployed: { address },
      },
      receipts,
    ];
  }

  async update(): Promise<AnnotatedTx[]> {
    return [];
  }
}

export class MidnightValidatorAnnounceArtifactManager implements IRawValidatorAnnounceArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(private readonly chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readValidatorAnnounce(
    address: string,
  ): Promise<DeployedRawValidatorAnnounceArtifact> {
    return this.createReader('validatorAnnounce').read(address);
  }

  // Announced storage locations per validator, in input order. Not part of
  // the artifact-manager interface; used by `validator check`.
  async getAnnouncedStorageLocations(
    address: string,
    validators: string[],
  ): Promise<string[][]> {
    const state = await this.client.fetchContractState(address);
    if (!state) {
      return validators.map(() => []);
    }
    const locations: string[][] = [];
    for (const validatorHex of validators) {
      const validator = hexToBytes(validatorHex);
      // The count/at circuits assert membership, so guard with isAnnounced —
      // an unknown validator yields an empty list instead of a throw.
      const announced = await this.client.runCircuit<boolean>(
        'validator-announce',
        state.data,
        'isAnnounced',
        [validator],
      );
      if (!announced) {
        locations.push([]);
        continue;
      }
      const count = await this.client.runCircuit<bigint>(
        'validator-announce',
        state.data,
        'locationCount',
        [validator],
      );
      const perValidator: string[] = [];
      for (let i = 0n; i < count; i++) {
        const buffer = await this.client.runCircuit<Uint8Array>(
          'validator-announce',
          state.data,
          'locationAt',
          [validator, i],
        );
        perValidator.push(decodeLocation(buffer));
      }
      locations.push(perValidator);
    }
    return locations;
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
    signer: ISigner<AnnotatedTx, TxReceipt>,
  ): ArtifactWriter<
    RawValidatorAnnounceArtifactConfigs[T],
    DeployedValidatorAnnounceAddress
  > {
    return new MidnightValidatorAnnounceWriter(
      this.client,
      this.chainMetadata,
      requireMidnightSigner(signer),
    );
  }
}
