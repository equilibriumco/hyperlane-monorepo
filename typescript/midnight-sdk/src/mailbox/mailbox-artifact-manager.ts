import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner } from '@hyperlane-xyz/provider-sdk/altvm';
import {
  ArtifactState,
  isArtifactDeployed,
  type ArtifactNew,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
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
import { NIGHT_ISM_SHAPE, assertNightIsItsOwnIsm } from '../utils/errors.js';
import type { MidnightTransaction } from '../utils/types.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { MidnightSigner, requireMidnightSigner } from '../clients/signer.js';
import {
  nightConstructorArgs,
  parseMaxTransferAmount,
} from '../deploy/night-args.js';
import {
  maxValidators,
  resolveValidatorSet,
  setValidatorsTx,
  validatorSetsByOrigin,
} from '../ism/validators.js';

// What the writer needs of the signer: the night deploy, and the owner's
// transactions for the origins the constructor cannot take.
export type NightDeployer = Pick<
  MidnightSigner,
  'deployMidnightContract' | 'sendAndConfirmTransaction'
>;

class MidnightMailboxReader implements ArtifactReader<
  RawMailboxArtifactConfigs['mailbox'],
  DeployedMailboxAddress
> {
  constructor(protected readonly client: MidnightReadClient) {}

  async read(address: string): Promise<DeployedRawMailboxArtifact> {
    const state = await this.client.requireContractState(address);
    const [owner, localDomain, maxTransfer] = await Promise.all([
      this.client.runCircuit<Uint8Array>('night', state.data, 'owner'),
      this.client.runCircuit<bigint>('night', state.data, 'localDomain'),
      this.client.runCircuit<bigint>('night', state.data, 'maxTransfer'),
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
        maxTransferAmount: maxTransfer.toString(),
      },
      deployed: { address, domainId: Number(localDomain) },
    };
  }
}

export class MidnightMailboxWriter
  extends MidnightMailboxReader
  implements ArtifactWriter<MailboxOnChain, DeployedMailboxAddress>
{
  constructor(
    client: MidnightReadClient,
    private readonly metadata: ChainMetadataForAltVM,
    private readonly signer: NightDeployer,
  ) {
    super(client);
  }

  // Deploys the night monolith: mailbox, ISM, and native warp route in one
  // contract. The ISM config is consumed here: the constructor takes one
  // origin's validators, the decimals and the transfer limit, and the owner
  // sets every other origin's validators right after. Hook placeholders from
  // the orchestrator are ignored.
  async create(
    artifact: ArtifactNew<MailboxOnChain>,
  ): Promise<[DeployedRawMailboxArtifact, TxReceipt[]]> {
    const config = artifact.config;
    const ism = config.defaultIsm;
    if (!isArtifactDeployed(ism) || ism.config.type !== 'domainRoutingIsm') {
      throw new Error(`night deploy needs its ISM inline: ${NIGHT_ISM_SHAPE}`);
    }
    const slots = await maxValidators();
    // Lowest origin first, so a config always deploys the same way. Every set
    // is resolved before the deploy, which nothing can undo.
    const [initial, ...others] = [...validatorSetsByOrigin(ism.config.domains)]
      .sort(([a], [b]) => a - b)
      .map(([origin, multisig]) => ({
        origin,
        set: resolveValidatorSet(multisig, slots),
      }));
    if (!initial) {
      throw new Error(
        `night needs the validators of at least one origin chain: ${NIGHT_ISM_SHAPE}`,
      );
    }

    const localDecimals = this.metadata.nativeToken?.decimals;
    if (localDecimals === undefined) {
      throw new Error(
        `chain metadata for ${this.metadata.name} has no nativeToken.decimals — ` +
          `required to seal the night contract's local decimals`,
      );
    }
    const domainId = BigInt(this.metadata.domainId);
    const maxTransferAmount = parseMaxTransferAmount(config.maxTransferAmount);

    const { address, ownerId, receipts } =
      await this.signer.deployMidnightContract({
        name: 'night',
        chunked: true,
        buildArgs: ({ ownerId, instanceSalt }) =>
          nightConstructorArgs({
            ownerId,
            instanceSalt,
            domainId,
            initialOrigin: BigInt(initial.origin),
            validatorSet: initial.set,
            localDecimals: BigInt(localDecimals),
            maxTransferAmount,
          }),
      });

    for (const { origin, set } of others) {
      receipts.push(
        await this.signer.sendAndConfirmTransaction(
          setValidatorsTx(address, origin, set),
        ),
      );
    }

    // The ISM artifacts still carry their pre-deploy placeholder address, and
    // the orchestrator reuses the same objects when reporting deployed
    // addresses and in its own update step. The night contract is its own ISM
    // for every origin, so stamp the real address on now.
    ism.deployed.address = address;
    for (const originIsm of Object.values(ism.config.domains)) {
      if (isArtifactDeployed(originIsm)) originIsm.deployed.address = address;
    }

    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: { ...config, owner: bytesToHex(ownerId) },
        deployed: { address, domainId: Number(domainId) },
      },
      receipts,
    ];
  }

  // The monolith's ISM/hook pointers are sealed at construction; the
  // mailbox-level mutable state is the transfer limit and ownership. Validator
  // rotation goes through the ISM writer.
  async update(
    artifact: DeployedRawMailboxArtifact,
  ): Promise<MidnightTransaction[]> {
    const address = artifact.deployed.address;
    assertNightIsItsOwnIsm(
      address,
      artifact.config.defaultIsm.deployed.address,
    );
    const current = await this.read(address);
    const txs: MidnightTransaction[] = [];

    // The limit is changed only when the config sets it, and before any
    // ownership transfer, which would take the signer's right to change it.
    if (artifact.config.maxTransferAmount !== undefined) {
      const expectedLimit = parseMaxTransferAmount(
        artifact.config.maxTransferAmount,
      );
      if (
        expectedLimit !==
        parseMaxTransferAmount(current.config.maxTransferAmount)
      ) {
        txs.push({
          annotation: `Set the night transfer limit to ${expectedLimit}`,
          contract: 'night',
          contractAddress: address,
          circuit: 'setMaxTransfer',
          args: [expectedLimit],
        });
      }
    }

    const expectedOwner = artifact.config.owner;
    if (!expectedOwner || expectedOwner === ZERO_ADDRESS_HEX_32) {
      return txs;
    }
    const ownerBytes = hexToBytes(expectedOwner);
    if (ownerBytes.length !== 32) {
      throw new Error(
        `night owner must be a 32-byte ZOwnablePK commitment, got ${expectedOwner}`,
      );
    }
    if (
      normalizeHexForCompare(current.config.owner) !==
      normalizeHexForCompare(expectedOwner)
    ) {
      txs.push({
        annotation: `Transfer night ownership to ${expectedOwner}`,
        contract: 'night',
        contractAddress: address,
        circuit: 'transferOwnership',
        args: [ownerBytes],
      });
    }
    return txs;
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
