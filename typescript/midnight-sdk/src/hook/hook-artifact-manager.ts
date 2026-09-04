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
  DeployedHookAddress,
  DeployedHookArtifact,
  HookType,
  IRawHookArtifactManager,
  RawHookArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/hook';
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
import { unsupportedOnMidnight } from '../utils/errors.js';
import type { MidnightTransaction, MidnightTxReceipt } from '../utils/types.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { MidnightSigner, requireMidnightSigner } from '../clients/signer.js';
import { readRemoteGasData, topLevelArity } from '../clients/state.js';

// Midnight has no dispatch-coupled hooks. This manager exists because the CLI
// models two other things as hooks: the checkpoint format's merkle_tree_hook
// identity, and the IGP, which here is a standalone contract paid in a separate
// transaction.
const NIGHT_STATE_ARITY = 2;
const IGP_STATE_ARITY = 8;

type EitherAddress = {
  is_left: boolean;
  left: { bytes: Uint8Array };
  right: { bytes: Uint8Array };
};

type IgpHookConfig = RawHookArtifactConfigs['interchainGasPaymaster'];
type MerkleTreeHookConfig = RawHookArtifactConfigs['merkleTreeHook'];

class MidnightMerkleTreeHookReader implements ArtifactReader<
  MerkleTreeHookConfig,
  DeployedHookAddress
> {
  async read(
    address: string,
  ): Promise<ArtifactDeployed<MerkleTreeHookConfig, DeployedHookAddress>> {
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: { type: 'merkleTreeHook' },
      deployed: { address },
    };
  }
}

// "Creating" the merkle tree hook deploys nothing: the merkle tree lives
// off-chain and the mailbox (night) address fills the checkpoint format's
// merkle_tree_hook slot — the artifact just records that identity.
class MidnightMerkleTreeHookWriter
  extends MidnightMerkleTreeHookReader
  implements ArtifactWriter<MerkleTreeHookConfig, DeployedHookAddress>
{
  constructor(private readonly mailbox: string | undefined) {
    super();
  }

  async create(
    artifact: ArtifactNew<MerkleTreeHookConfig>,
  ): Promise<
    [ArtifactDeployed<MerkleTreeHookConfig, DeployedHookAddress>, TxReceipt[]]
  > {
    if (!this.mailbox) {
      throw new Error(
        'merkleTreeHook on Midnight is the mailbox address itself — the hook ' +
          'manager needs the mailbox context to derive it',
      );
    }
    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: artifact.config,
        deployed: { address: this.mailbox },
      },
      [],
    ];
  }

  async update(): Promise<MidnightTransaction[]> {
    return [];
  }
}

class MidnightIgpHookReader implements ArtifactReader<
  IgpHookConfig,
  DeployedHookAddress
> {
  constructor(protected readonly client: MidnightReadClient) {}

  async read(
    address: string,
  ): Promise<ArtifactDeployed<IgpHookConfig, DeployedHookAddress>> {
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
      overhead[entry.domainId] = Number(entry.gasOverhead);
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

class MidnightIgpHookWriter
  extends MidnightIgpHookReader
  implements ArtifactWriter<IgpHookConfig, DeployedHookAddress>
{
  constructor(
    client: MidnightReadClient,
    private readonly signer: MidnightSigner,
  ) {
    super(client);
  }

  async create(
    artifact: ArtifactNew<IgpHookConfig>,
  ): Promise<
    [ArtifactDeployed<IgpHookConfig, DeployedHookAddress>, TxReceipt[]]
  > {
    const config = artifact.config;
    assertOverheadDomainsHaveOracles(config);
    const receipts: MidnightTxReceipt[] = [];

    const deployResult = await this.signer.deployMidnightContract({
      name: 'igp',
      buildArgs: ({ ownerId, instanceSalt, deployerUnshielded }) => {
        // The sealed `claim` beneficiary, treated as an unshielded user
        // address. Falls back to the deployer's own, and `setBeneficiary`
        // re-points it later.
        const beneficiary = resolveBeneficiaryBytes(
          config.beneficiary,
          deployerUnshielded,
        );
        return [
          ownerId,
          instanceSalt,
          {
            is_left: false,
            left: { bytes: new Uint8Array(32) },
            right: { bytes: beneficiary },
          },
        ];
      },
    });
    receipts.push(...deployResult.receipts);
    const { address, ownerId } = deployResult;

    for (const [domain, oracle] of sortedOracleEntries(config)) {
      const tx: MidnightTransaction = {
        annotation: `Set IGP gas oracle for domain ${domain}`,
        contract: 'igp',
        contractAddress: address,
        circuit: 'setRemoteGasData',
        args: [
          BigInt(domain),
          BigInt(oracle.tokenExchangeRate),
          BigInt(oracle.gasPrice),
          BigInt(config.overhead?.[domain] ?? 0),
        ],
      };
      receipts.push(await this.signer.sendAndConfirmTransaction(tx));
    }

    const ownerHex = bytesToHex(ownerId);
    if (
      config.owner &&
      config.owner !== ZERO_ADDRESS_HEX_32 &&
      normalizeHexForCompare(config.owner) !== normalizeHexForCompare(ownerHex)
    ) {
      receipts.push(
        await this.signer.sendAndConfirmTransaction({
          annotation: `Transfer IGP ownership to ${config.owner}`,
          contract: 'igp',
          contractAddress: address,
          circuit: 'transferOwnership',
          args: [requireCommitment(config.owner, 'IGP owner')],
        }),
      );
    }

    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: { ...config, owner: config.owner || ownerHex },
        deployed: { address },
      },
      receipts,
    ];
  }

  async update(
    artifact: ArtifactDeployed<IgpHookConfig, DeployedHookAddress>,
  ): Promise<MidnightTransaction[]> {
    const expected = artifact.config;
    assertOverheadDomainsHaveOracles(expected);
    const address = artifact.deployed.address;
    const current = (await this.read(address)).config;
    const txs: MidnightTransaction[] = [];

    for (const [domain, oracle] of sortedOracleEntries(expected)) {
      const existing = current.oracleConfig[domain];
      const expectedOverhead = BigInt(expected.overhead?.[domain] ?? 0);
      if (
        existing &&
        BigInt(existing.gasPrice) === BigInt(oracle.gasPrice) &&
        BigInt(existing.tokenExchangeRate) ===
          BigInt(oracle.tokenExchangeRate) &&
        BigInt(current.overhead?.[domain] ?? 0) === expectedOverhead
      ) {
        continue;
      }
      txs.push({
        annotation: `Set IGP gas oracle for domain ${domain}`,
        contract: 'igp',
        contractAddress: address,
        circuit: 'setRemoteGasData',
        args: [
          BigInt(domain),
          BigInt(oracle.tokenExchangeRate),
          BigInt(oracle.gasPrice),
          expectedOverhead,
        ],
      });
    }

    if (
      expected.beneficiary &&
      normalizeHexForCompare(expected.beneficiary) !==
        normalizeHexForCompare(current.beneficiary)
    ) {
      txs.push({
        annotation: `Set IGP beneficiary to ${expected.beneficiary}`,
        contract: 'igp',
        contractAddress: address,
        circuit: 'setBeneficiary',
        args: [
          {
            is_left: false,
            left: { bytes: new Uint8Array(32) },
            right: {
              bytes: requireCommitment(expected.beneficiary, 'IGP beneficiary'),
            },
          },
        ],
      });
    }

    if (
      expected.owner &&
      expected.owner !== ZERO_ADDRESS_HEX_32 &&
      normalizeHexForCompare(expected.owner) !==
        normalizeHexForCompare(current.owner)
    ) {
      txs.push({
        annotation: `Transfer IGP ownership to ${expected.owner}`,
        contract: 'igp',
        contractAddress: address,
        circuit: 'transferOwnership',
        args: [requireCommitment(expected.owner, 'IGP owner')],
      });
    }

    return txs;
  }
}

// Overhead is stored with the oracle pair, and the contract rejects a zero
// exchangeRate/gasPrice, so an overhead-only domain can never be written.
function assertOverheadDomainsHaveOracles(config: IgpHookConfig): void {
  const orphaned = Object.entries(config.overhead ?? {}).filter(
    ([domain, v]) => Number(v) !== 0 && !config.oracleConfig?.[Number(domain)],
  );
  if (orphaned.length > 0) {
    throw new Error(
      `IGP overhead set for domains with no oracleConfig entry (overhead is ` +
        `stored with the oracle pair): ` +
        orphaned.map(([d]) => d).join(', '),
    );
  }
}

function sortedOracleEntries(
  config: IgpHookConfig,
): Array<[number, { gasPrice: string; tokenExchangeRate: string }]> {
  return Object.entries(config.oracleConfig ?? {})
    .map(
      ([domain, oracle]) =>
        [Number(domain), oracle] as [
          number,
          { gasPrice: string; tokenExchangeRate: string },
        ],
    )
    .sort(([a], [b]) => a - b);
}

function resolveBeneficiaryBytes(
  beneficiary: string | undefined,
  fallback: Uint8Array,
): Uint8Array {
  if (!beneficiary || beneficiary === ZERO_ADDRESS_HEX_32) {
    return fallback;
  }
  return requireCommitment(beneficiary, 'IGP beneficiary');
}

function requireCommitment(hex: string, label: string): Uint8Array {
  const bytes = hexToBytes(hex);
  if (bytes.length !== 32) {
    throw new Error(`${label} must be 32 bytes, got ${hex}`);
  }
  return bytes;
}

export class MidnightHookArtifactManager implements IRawHookArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(
    chainMetadata: ChainMetadataForAltVM,
    private readonly context?: { mailbox?: string },
  ) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readHook(address: string): Promise<DeployedHookArtifact> {
    const state = await this.client.requireContractState(address);
    const arity = topLevelArity(state.data);
    switch (arity) {
      case NIGHT_STATE_ARITY:
        return this.createReader('merkleTreeHook').read(address);
      case IGP_STATE_ARITY:
        return this.createReader('interchainGasPaymaster').read(address);
      default:
        throw new Error(
          `contract at ${address} is neither the night monolith nor the igp (top-level state arity ${arity})`,
        );
    }
  }

  createReader<T extends HookType>(
    type: T,
  ): ArtifactReader<RawHookArtifactConfigs[T], DeployedHookAddress> {
    const readers: {
      [K in HookType]: () => ArtifactReader<
        RawHookArtifactConfigs[K],
        DeployedHookAddress
      >;
    } = {
      merkleTreeHook: () => new MidnightMerkleTreeHookReader(),
      interchainGasPaymaster: () => new MidnightIgpHookReader(this.client),
      protocolFee: unsupportedOnMidnight('hook', 'protocolFee'),
      unknownHook: unsupportedOnMidnight('hook', 'unknownHook'),
    };
    return readers[type]();
  }

  createWriter<T extends HookType>(
    type: T,
    signer: ISigner<AnnotatedTx, TxReceipt>,
  ): ArtifactWriter<RawHookArtifactConfigs[T], DeployedHookAddress> {
    const writers: {
      [K in HookType]: () => ArtifactWriter<
        RawHookArtifactConfigs[K],
        DeployedHookAddress
      >;
    } = {
      merkleTreeHook: () =>
        new MidnightMerkleTreeHookWriter(this.context?.mailbox),
      interchainGasPaymaster: () =>
        new MidnightIgpHookWriter(this.client, requireMidnightSigner(signer)),
      protocolFee: unsupportedOnMidnight('hook', 'protocolFee'),
      unknownHook: unsupportedOnMidnight('hook', 'unknownHook'),
    };
    return writers[type]();
  }
}
