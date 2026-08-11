import * as os from 'node:os';
import * as path from 'node:path';

import { CompiledContract } from '@midnight-ntwrk/compact-js';
import { findDeployedContract } from '@midnight-ntwrk/midnight-js-contracts';
import { setNetworkId } from '@midnight-ntwrk/midnight-js-network-id';
import { nativeToken } from '@midnightntwrk/ledger-v9';

import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner, ReqGetBalance } from '@hyperlane-xyz/provider-sdk/altvm';
import { retryAsync } from '@hyperlane-xyz/utils';

import {
  artifactsPathFor,
  deployContractInstance,
} from '../deploy/deploy-contract.js';
import {
  OwnerStateStore,
  decodeCoinPublicKey,
  userPkEither,
} from '../deploy/owner-state.js';
import {
  createProviders,
  type ContractProviders,
} from '../deploy/providers.js';
import {
  createWallet,
  deriveUnshieldedKeystore,
  getUnshieldedAddress,
  registerNightForDust,
  syncedCoinPublicKeyHex,
  waitForSync,
  type WalletContext,
  type WalletEndpoints,
} from '../deploy/wallet.js';
import { bytesToHex, hexToBytes } from '../utils/conversion.js';
import type { MidnightTransaction, MidnightTxReceipt } from '../utils/types.js';

import {
  loadContractModule,
  witnessesFor,
  type MidnightContractName,
} from './contracts.js';
import { MidnightIndexerClient } from './indexer.js';
import { MidnightProvider } from './provider.js';
import { MidnightReadClient } from './read-client.js';

// Proving is client-side in Midnight's architecture; public networks expose
// no proof server, so the default is the operator's own local instance.
const DEFAULT_PROOF_SERVER_URL = 'http://127.0.0.1:6300';
// Local private-state DB password (SDK enforces length/charset rules);
// override via MIDNIGHT_STATE_PASSWORD.
const DEFAULT_PRIVATE_STATE_PASSWORD = 'Hyperlane-Midnight-2026!';

export interface MidnightSignerOptions {
  seedHex: string;
  endpoints: WalletEndpoints;
  stateDir: string;
  privateStatePassword: string;
}

export class MidnightSigner
  extends MidnightProvider
  implements ISigner<MidnightTransaction, MidnightTxReceipt>
{
  readonly ownerStore: OwnerStateStore;
  private readonly signerAddress: string;
  private walletPromise: Promise<WalletContext> | null = null;
  private readonly providersByContract = new Map<
    MidnightContractName,
    Promise<ContractProviders>
  >();

  protected constructor(
    metadata: ChainMetadataForAltVM,
    client: MidnightReadClient,
    indexer: MidnightIndexerClient,
    private readonly options: MidnightSignerOptions,
  ) {
    super(metadata, client, indexer);
    this.ownerStore = new OwnerStateStore(options.stateDir);
    this.signerAddress = String(
      deriveUnshieldedKeystore(options.seedHex).getBech32Address(),
    );
  }

  static async connectWithSigner(
    metadata: ChainMetadataForAltVM,
    privateKey: string,
  ): Promise<MidnightSigner> {
    const seedHex = privateKey.startsWith('0x')
      ? privateKey.slice(2)
      : privateKey;
    if (!/^[0-9a-fA-F]{64}$/.test(seedHex)) {
      throw new Error(
        'Midnight signer key must be a hex-encoded 32-byte wallet seed (64 hex chars)',
      );
    }

    // The ledger/wallet stack encodes addresses and transactions against a
    // process-global network id; it must be set before any key derivation.
    const networkId = (metadata as { midnightNetworkId?: string })
      .midnightNetworkId;
    if (!networkId) {
      throw new Error(
        `chain metadata for ${metadata.name} has no midnightNetworkId — ` +
          `set it to the Midnight network id (e.g. 'stagenet', or 'undeployed' for a local devnet)`,
      );
    }
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    setNetworkId(networkId as any);

    const client = MidnightReadClient.fromMetadata(metadata);
    const endpoints: WalletEndpoints = {
      indexer: client.endpoints.indexerGraphqlUrl,
      indexerWs: client.endpoints.indexerWsUrl,
      node: client.endpoints.nodeUrl,
      proofServer:
        metadata.proofServerUrl ??
        process.env.MIDNIGHT_PROOF_SERVER_URL ??
        DEFAULT_PROOF_SERVER_URL,
    };
    const stateDir =
      process.env.MIDNIGHT_STATE_DIR ??
      path.join(os.homedir(), '.hyperlane-midnight', metadata.name);
    const privateStatePassword =
      process.env.MIDNIGHT_STATE_PASSWORD ?? DEFAULT_PRIVATE_STATE_PASSWORD;

    return new MidnightSigner(
      metadata,
      client,
      new MidnightIndexerClient(client.endpoints.indexerGraphqlUrl),
      { seedHex, endpoints, stateDir, privateStatePassword },
    );
  }

  getSignerAddress(): string {
    return this.signerAddress;
  }

  supportsTransactionBatching(): boolean {
    return false;
  }

  async transactionToPrintableJson(
    transaction: MidnightTransaction,
  ): Promise<object> {
    return toPrintable(transaction) as object;
  }

  async sendAndConfirmTransaction(
    transaction: MidnightTransaction,
  ): Promise<MidnightTxReceipt> {
    const deployed = await this.joinContract(
      transaction.contract,
      transaction.contractAddress,
    );
    const call = deployed.callTx[transaction.circuit];
    if (!call) {
      throw new Error(
        `circuit ${transaction.circuit} not found on ${transaction.contract} ` +
          `at ${transaction.contractAddress}`,
      );
    }
    const txData = await call(...transaction.args);
    const receipt: MidnightTxReceipt = {
      txId: String(txData.public.txId),
      blockHeight: Number(txData.public.blockHeight ?? 0),
    };

    // transferRemote returns the dispatched messageId as its circuit
    // result; the payForGas follow-up needs it, and core adapters read it
    // off the receipt (there is no dispatch log to parse).
    const result = txData.private?.result;
    if (result instanceof Uint8Array && result.length === 32) {
      receipt.messageId = bytesToHex(result);
      if (transaction.circuit === 'transferRemote') {
        receipt.destinationDomainId = Number(transaction.args[0]);
      }
    }

    if (transaction.payForGas) {
      if (!receipt.messageId) {
        throw new Error(
          `${transaction.circuit} returned no messageId; cannot run the payForGas follow-up`,
        );
      }
      const gas = transaction.payForGas;
      try {
        const paid = await retryAsync(
          () =>
            this.payForGas({
              igpAddress: gas.igpAddress,
              messageId: receipt.messageId!,
              destinationDomainId: gas.destinationDomainId,
              gasLimit: gas.gasLimit,
              amount: gas.amount,
            }),
          3,
          1000,
        );
        receipt.payForGasTxId = paid.txId;
      } catch (err) {
        // The dispatch already landed; the message is valid but unpaid and a
        // relayer running a payment policy will withhold it. payForGas is
        // permissionless, so the payment below completes it at any time.
        throw new Error(
          `message ${receipt.messageId} dispatched (tx ${receipt.txId}) but its gas payment ` +
            `failed after retries — complete it with payForGas(igpAddress: ${gas.igpAddress}, ` +
            `messageId: ${receipt.messageId}, destinationDomainId: ${gas.destinationDomainId}, ` +
            `gasLimit: ${gas.gasLimit}, amount: ${gas.amount}): ${
              err instanceof Error ? err.message : String(err)
            }`,
        );
      }
    }

    return receipt;
  }

  // Standalone, permissionless gas payment for an already-dispatched
  // message: the rescue path when the follow-up above failed, and the top-up
  // path when a relayer's policy needs more than was originally paid.
  async payForGas(req: {
    igpAddress: string;
    messageId: string;
    destinationDomainId: number;
    gasLimit: string | bigint;
    amount: string | bigint;
  }): Promise<MidnightTxReceipt> {
    const messageId = hexToBytes(req.messageId);
    if (messageId.length !== 32) {
      throw new Error(`messageId must be 32 bytes, got ${req.messageId}`);
    }
    const igp = await this.joinContract('igp', req.igpAddress);
    const txData = await igp.callTx.payForGas(
      messageId,
      BigInt(req.destinationDomainId),
      BigInt(req.gasLimit),
      BigInt(req.amount),
    );
    return {
      txId: String(txData.public.txId),
      blockHeight: Number(txData.public.blockHeight ?? 0),
    };
  }

  async sendAndConfirmBatchTransactions(
    _transactions: MidnightTransaction[],
  ): Promise<MidnightTxReceipt> {
    throw new Error('MidnightSigner does not support transaction batching');
  }

  // Wallet (mn...) addresses need a synced wallet; the signer has one for
  // its own address. Contract addresses fall through to the read path.
  override async getBalance(req: ReqGetBalance): Promise<bigint> {
    if (req.address === this.signerAddress) {
      const ctx = await this.walletContext();
      const state = await waitForSync(ctx);
      return state.unshielded?.balances[nativeToken().raw] ?? 0n;
    }
    return super.getBalance(req);
  }

  /**
   * Deploy one of the compiled contracts. Computes the ZOwnablePK owner
   * commitment from the wallet's shielded coinPublicKey plus the persisted
   * (or freshly generated) per-contract secretNonce, then runs the deploy —
   * chunked for the night monolith, single-shot otherwise.
   */
  async deployMidnightContract(options: {
    name: MidnightContractName;
    chunked?: boolean;
    buildArgs: (ctx: {
      ownerId: Uint8Array;
      instanceSalt: Uint8Array;
      deployerUnshielded: Uint8Array;
    }) => unknown[];
    log?: (message: string) => void;
  }): Promise<{
    address: string;
    ownerId: Uint8Array;
    receipts: MidnightTxReceipt[];
  }> {
    const log = options.log ?? defaultLog;
    const wallet = await this.walletContext();
    const providers = await this.providersFor(options.name);
    const module = await loadContractModule(options.name);

    const owner = this.ownerStore.getOrCreate(options.name);
    const coinPk = decodeCoinPublicKey(await syncedCoinPublicKeyHex(wallet));
    const ownerId = module.pureCircuits.computeOwnerId(
      userPkEither(coinPk),
      owner.secretNonce,
    );
    const deployerUnshielded = new Uint8Array(
      (await getUnshieldedAddress(wallet)).data,
    );

    const { address, receipts } = await deployContractInstance({
      name: options.name,
      args: options.buildArgs({
        ownerId,
        instanceSalt: owner.instanceSalt,
        deployerUnshielded,
      }),
      secretNonce: owner.secretNonce,
      providers,
      chunked: options.chunked ?? false,
      ownerStore: this.ownerStore,
      log,
    });
    return { address, ownerId, receipts };
  }

  private walletContext(): Promise<WalletContext> {
    if (!this.walletPromise) {
      this.walletPromise = (async () => {
        const ctx = await createWallet(
          this.options.seedHex,
          this.options.endpoints,
        );
        await this.ensureFeesReady(ctx);
        return ctx;
      })();
    }
    return this.walletPromise;
  }

  // Fees are paid in DUST generated from registered NIGHT UTXOs. Registers
  // unregistered UTXOs (idempotent) and waits for a positive DUST balance;
  // an empty wallet fails fast instead of waiting forever.
  private async ensureFeesReady(ctx: WalletContext): Promise<void> {
    const state = await waitForSync(ctx);
    const dust = state.dust?.balance(new Date()) ?? 0n;
    if (dust > 0n) return;
    const night = state.unshielded?.balances[nativeToken().raw] ?? 0n;
    if (night === 0n) {
      throw new Error(
        `signer wallet ${this.signerAddress} holds no NIGHT — fund it before ` +
          `submitting transactions (fees are paid in DUST generated from NIGHT)`,
      );
    }
    await registerNightForDust(ctx);
  }

  private providersFor(name: MidnightContractName): Promise<ContractProviders> {
    let cached = this.providersByContract.get(name);
    if (!cached) {
      cached = this.walletContext().then((wallet) =>
        createProviders(wallet, artifactsPathFor(name), {
          endpoints: this.options.endpoints,
          privateStateDbPath: this.ownerStore.privateStateStorePath(),
          privateStatePassword: this.options.privateStatePassword,
        }),
      );
      this.providersByContract.set(name, cached);
    }
    return cached;
  }

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  private readonly joinedContracts = new Map<string, Promise<any>>();

  // Join a deployed contract for proven circuit calls. The stored private
  // state (from a local deploy) wins over initialPrivateState; on a fresh
  // machine the persisted owner-state nonce restores the owner identity.
  private joinContract(
    name: MidnightContractName,
    rawContractAddress: string,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  ): Promise<any> {
    // Registry configs carry 0x-prefixed addresses; the ledger's address
    // parser rejects the prefix.
    const contractAddress = rawContractAddress.startsWith('0x')
      ? rawContractAddress.slice(2)
      : rawContractAddress;
    const key = `${name}:${contractAddress}`;
    let cached = this.joinedContracts.get(key);
    if (!cached) {
      cached = (async () => {
        const providers = await this.providersFor(name);
        const module = await loadContractModule(name);
        const secretNonce =
          this.ownerStore.get(name)?.secretNonce ?? new Uint8Array(32);
        /* eslint-disable @typescript-eslint/no-explicit-any */
        const compiledContract = (CompiledContract.make as any)(
          name,
          module.Contract,
        ).pipe(
          (CompiledContract.withWitnesses as any)(witnessesFor(name)),
          (CompiledContract.withCompiledFileAssets as any)(
            artifactsPathFor(name),
          ),
        );
        return await (findDeployedContract as any)(providers, {
          compiledContract,
          contractAddress,
          privateStateId: `${name}-state`,
          initialPrivateState: { secretNonce },
        });
        /* eslint-enable @typescript-eslint/no-explicit-any */
      })();
      this.joinedContracts.set(key, cached);
    }
    return cached;
  }
}

// Midnight writers need signer capabilities beyond the ISigner interface
// (contract deploys, owner state); same downcast pattern as the other
// protocol SDKs.
export function requireMidnightSigner(signer: unknown): MidnightSigner {
  if (!(signer instanceof MidnightSigner)) {
    throw new Error(
      'expected a MidnightSigner (created by MidnightProtocolProvider.createSigner)',
    );
  }
  return signer;
}

function defaultLog(message: string): void {
  // eslint-disable-next-line no-console
  console.log(`midnight: ${message}`);
}

function toPrintable(value: unknown): unknown {
  if (typeof value === 'bigint') return value.toString();
  if (value instanceof Uint8Array) {
    return `0x${Buffer.from(value).toString('hex')}`;
  }
  if (Array.isArray(value)) return value.map(toPrintable);
  if (value !== null && typeof value === 'object') {
    return Object.fromEntries(
      Object.entries(value).map(([k, v]) => [k, toPrintable(v)]),
    );
  }
  return value;
}
