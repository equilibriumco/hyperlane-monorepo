import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import {
  ArtifactState,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedMailboxAddress,
  DeployedRawMailboxArtifact,
  IRawMailboxArtifactManager,
  MailboxType,
  RawMailboxArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/mailbox';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { bytesToHex } from '../utils/conversion.js';
import { MidnightReadClient } from '../clients/read-client.js';

class MidnightMailboxReader implements ArtifactReader<
  RawMailboxArtifactConfigs['mailbox'],
  DeployedMailboxAddress
> {
  constructor(private readonly client: MidnightReadClient) {}

  async read(address: string): Promise<DeployedRawMailboxArtifact> {
    const state = await this.client.requireContractState(address);
    const [owner, localDomain] = await Promise.all([
      this.client.runCircuit<Uint8Array>('night', state.data, 'owner'),
      this.client.runCircuit<bigint>('night', state.data, 'localDomain'),
    ]);
    // The night contract is a monolith and is its own default ISM. Midnight
    // has no dispatch-coupled hooks: the merkle tree lives off-chain
    // (validators rebuild it from dispatched_messages), and the night
    // address fills the checkpoint format's merkle_tree_hook slot — so the
    // "defaultHook" here is that protocol identity, not a contract. There
    // is no required hook.
    const self = {
      artifactState: ArtifactState.UNDERIVED,
      deployed: { address },
    } as const;
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: {
        owner: bytesToHex(owner),
        defaultIsm: self,
        defaultHook: self,
        requiredHook: {
          artifactState: ArtifactState.UNDERIVED,
          deployed: { address: ZERO_ADDRESS_HEX_32 },
        },
      },
      deployed: { address, domainId: Number(localDomain) },
    };
  }
}

export class MidnightMailboxArtifactManager implements IRawMailboxArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readMailbox(address: string): Promise<DeployedRawMailboxArtifact> {
    return this.createReader('mailbox').read(address);
  }

  createReader<T extends MailboxType>(
    _type: T,
  ): ArtifactReader<RawMailboxArtifactConfigs[T], DeployedMailboxAddress> {
    return new MidnightMailboxReader(this.client);
  }

  createWriter<T extends MailboxType>(
    _type: T,
  ): ArtifactWriter<RawMailboxArtifactConfigs[T], DeployedMailboxAddress> {
    throw new Error(
      'MidnightMailboxArtifactManager.createWriter: not implemented yet (#105)',
    );
  }
}
