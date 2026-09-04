import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner } from '@hyperlane-xyz/provider-sdk/altvm';
import {
  ArtifactState,
  type ArtifactNew,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type { RawIsmArtifactConfigs } from '@hyperlane-xyz/provider-sdk/ism';
import type {
  DeployedMailboxAddress,
  DeployedRawMailboxArtifact,
  IRawMailboxArtifactManager,
  MailboxOnChain,
  MailboxType,
  RawMailboxArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/mailbox';
import type {
  AnnotatedTx,
  TxReceipt,
} from '@hyperlane-xyz/provider-sdk/module';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import {
  bytesToHex,
  hexToBytes,
  normalizeHexForCompare,
} from '../utils/conversion.js';
import type { MidnightTransaction } from '../utils/types.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { MidnightSigner, requireMidnightSigner } from '../clients/signer.js';
import { resolveValidatorSet } from '../ism/validators.js';

// Wire-format precision of warp transfer amounts, meaning the max decimals in
// the route rather than the remote chain's local token decimals. Check the
// contracts repo's Scale module before changing this.
const MESSAGE_DECIMALS = 18n;

class MidnightMailboxReader implements ArtifactReader<
  RawMailboxArtifactConfigs['mailbox'],
  DeployedMailboxAddress
> {
  constructor(protected readonly client: MidnightReadClient) {}

  async read(address: string): Promise<DeployedRawMailboxArtifact> {
    const state = await this.client.requireContractState(address);
    const [owner, localDomain] = await Promise.all([
      this.client.runCircuit<Uint8Array>('night', state.data, 'owner'),
      this.client.runCircuit<bigint>('night', state.data, 'localDomain'),
    ]);
    // The night contract is a monolith and its own default ISM. There are no
    // dispatch-coupled hooks here, so `defaultHook` is the merkle_tree_hook
    // identity the checkpoint format wants, not a contract, and nothing fills
    // the required hook.
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

class MidnightMailboxWriter
  extends MidnightMailboxReader
  implements ArtifactWriter<MailboxOnChain, DeployedMailboxAddress>
{
  constructor(
    client: MidnightReadClient,
    private readonly metadata: ChainMetadataForAltVM,
    private readonly signer: MidnightSigner,
  ) {
    super(client);
  }

  // Deploys the night monolith: mailbox, ISM, and native warp route in one
  // contract. The multisig ISM config is consumed here, since validators,
  // threshold, and decimals are constructor args sealed at deploy time. Hook
  // placeholders from the orchestrator are ignored.
  async create(
    artifact: ArtifactNew<MailboxOnChain>,
  ): Promise<[DeployedRawMailboxArtifact, TxReceipt[]]> {
    const config = artifact.config;
    const ismConfig = this.extractMultisigConfig(config.defaultIsm);
    const validatorSet = resolveValidatorSet(ismConfig);

    const localDecimals = this.metadata.nativeToken?.decimals;
    if (localDecimals === undefined) {
      throw new Error(
        `chain metadata for ${this.metadata.name} has no nativeToken.decimals — ` +
          `required to seal the night contract's local decimals`,
      );
    }
    const domainId = BigInt(this.metadata.domainId);

    const { address, ownerId, receipts } =
      await this.signer.deployMidnightContract({
        name: 'night',
        chunked: true,
        buildArgs: ({ ownerId, instanceSalt }) => [
          ownerId,
          instanceSalt,
          domainId,
          validatorSet.paddedPubkeys,
          validatorSet.count,
          validatorSet.threshold,
          BigInt(localDecimals),
          MESSAGE_DECIMALS,
        ],
      });

    // The ISM artifact still carries its pre-deploy placeholder address, and the
    // orchestrator reuses that same object when reporting deployed addresses.
    // The night contract is its own ISM, so stamp the real address on now.
    config.defaultIsm.deployed.address = address;

    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: {
          ...config,
          owner: bytesToHex(ownerId),
          defaultIsm: {
            artifactState: ArtifactState.DEPLOYED,
            config: ismConfig,
            deployed: { address },
          },
        },
        deployed: { address, domainId: Number(domainId) },
      },
      receipts,
    ];
  }

  // The monolith's ISM/hook pointers are sealed at construction; the only
  // mailbox-level mutable state is ownership. Validator rotation goes
  // through the ISM writer.
  async update(
    artifact: DeployedRawMailboxArtifact,
  ): Promise<MidnightTransaction[]> {
    const address = artifact.deployed.address;
    const expectedOwner = artifact.config.owner;
    if (!expectedOwner || expectedOwner === ZERO_ADDRESS_HEX_32) {
      return [];
    }
    const ownerBytes = hexToBytes(expectedOwner);
    if (ownerBytes.length !== 32) {
      throw new Error(
        `night owner must be a 32-byte ZOwnablePK commitment, got ${expectedOwner}`,
      );
    }
    const current = await this.read(address);
    if (
      normalizeHexForCompare(current.config.owner) ===
      normalizeHexForCompare(expectedOwner)
    ) {
      return [];
    }
    return [
      {
        annotation: `Transfer night ownership to ${expectedOwner}`,
        contract: 'night',
        contractAddress: address,
        circuit: 'transferOwnership',
        args: [ownerBytes],
      },
    ];
  }

  private extractMultisigConfig(
    defaultIsm: MailboxOnChain['defaultIsm'],
  ): RawIsmArtifactConfigs['messageIdMultisigIsm'] {
    if (!('config' in defaultIsm) || !defaultIsm.config) {
      throw new Error(
        `night deploy needs the multisig ISM config inline (validators, ` +
          `threshold, validatorPubkeys) — a pre-deployed ISM address cannot ` +
          `be sealed into a new night instance`,
      );
    }
    const config = defaultIsm.config;
    if (config.type !== 'messageIdMultisigIsm') {
      throw new Error(
        `night's ISM is messageIdMultisigIsm, got ${config.type}`,
      );
    }
    return config;
  }
}

export class MidnightMailboxArtifactManager implements IRawMailboxArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(private readonly chainMetadata: ChainMetadataForAltVM) {
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
    signer: ISigner<AnnotatedTx, TxReceipt>,
  ): ArtifactWriter<RawMailboxArtifactConfigs[T], DeployedMailboxAddress> {
    return new MidnightMailboxWriter(
      this.client,
      this.chainMetadata,
      requireMidnightSigner(signer),
    );
  }
}
