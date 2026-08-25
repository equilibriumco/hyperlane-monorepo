import * as os from 'node:os';
import * as path from 'node:path';

import { CompiledContract } from '@midnight-ntwrk/compact-js';
import { findDeployedContract } from '@midnight-ntwrk/midnight-js-contracts';
import { setNetworkId } from '@midnight-ntwrk/midnight-js-network-id';
import { nativeToken } from '@midnightntwrk/ledger-v9';

import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type {
  ISigner,
  ReqGetBalance,
  ResFeeTokenBalance,
} from '@hyperlane-xyz/provider-sdk/altvm';
import { sleep } from '@hyperlane-xyz/utils';

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
  minimumDustSpecks,
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
import {
  findLandedGasPayment,
  payForGasWithLandingCheck,
  type LandedGasPayment,
} from './gas-payment.js';
import { MidnightIndexerClient } from './indexer.js';
import { MidnightProvider } from './provider.js';
import { MidnightReadClient } from './read-client.js';
import { readGasPayments } from './state.js';

// Proving is client-side and public networks expose no proof server, so the
// default is the operator's own local instance.
const DEFAULT_PROOF_SERVER_URL = 'http://127.0.0.1:6300';
// Local private-state DB password; the SDK enforces length and charset rules.
const DEFAULT_PRIVATE_STATE_PASSWORD = 'Hyperlane-Midnight-2026!';

const PAY_FOR_GAS_ATTEMPTS = 3;
const PAY_FOR_GAS_RETRY_DELAY_MS = 1000;
// Long enough to outlast normal indexer lag. It cannot prove a payment will
// never land: a broadcast transaction stays valid for the 30-minute tx TTL.
const PAY_FOR_GAS_SETTLE_MS_DEFAULT = 180_000;
const PAY_FOR_GAS_POLL_MS = 3000;

function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// Rejects bad values rather than passing them on: `Number('')` is 0, which
// disables the wait, and `Number('abc')` is NaN, which never hits a deadline.
function payForGasSettleMs(): number {
  const raw = process.env.MIDNIGHT_PAY_FOR_GAS_SETTLE_MS;
  if (raw === undefined || raw.trim() === '') {
    return PAY_FOR_GAS_SETTLE_MS_DEFAULT;
  }
  const parsed = Number(raw);
  if (!Number.isFinite(parsed) || parsed <= 0) {
    throw new Error(
      'MIDNIGHT_PAY_FOR_GAS_SETTLE_MS must be a positive number of ' +
        `milliseconds, got '${raw}'`,
    );
  }
  return parsed;
}

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
    setNetworkId(networkId);

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

    // transferRemote returns the dispatched messageId as its circuit result.
    // The payForGas follow-up needs it, and core adapters read it off the
    // receipt since there is no dispatch log to parse.
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
      const outcome = await payForGasWithLandingCheck({
        pay: () =>
          this.payForGas({
            igpAddress: gas.igpAddress,
            messageId: receipt.messageId!,
            destinationDomainId: gas.destinationDomainId,
            gasLimit: gas.gasLimit,
            amount: gas.amount,
          }),
        findLanded: () =>
          this.awaitLandedGasPayment(
            gas.igpAddress,
            receipt.messageId!,
            BigInt(gas.amount),
          ),
        attempts: PAY_FOR_GAS_ATTEMPTS,
        delayMs: PAY_FOR_GAS_RETRY_DELAY_MS,
        sleep,
      });

      if (outcome.kind === 'paid') {
        receipt.payForGasTxId = outcome.txId;
      } else if (outcome.kind === 'recovered') {
        receipt.payForGasIndex = outcome.index;
      } else {
        // The message is dispatched but unpaid, so a relayer with a payment
        // policy withholds it. payForGas is permissionless and works later.
        const rescue =
          `payForGas(igpAddress: ${gas.igpAddress}, messageId: ${receipt.messageId}, ` +
          `destinationDomainId: ${gas.destinationDomainId}, gasLimit: ${gas.gasLimit}, ` +
          `amount: ${gas.amount})`;
        const cause = errorText(outcome.error);
        const verifyFirst =
          `check IGP ${gas.igpAddress} for a recorded gas payment against ` +
          `messageId ${receipt.messageId} before running ${rescue}, or a ` +
          `payment that landed late is paid twice`;
        throw new Error(
          outcome.absence === 'not-seen'
            ? `message ${receipt.messageId} dispatched (tx ${receipt.txId}) but its gas payment ` +
                `failed and none was seen for it within the settle window — ${verifyFirst}: ${cause}`
            : `message ${receipt.messageId} dispatched (tx ${receipt.txId}) but whether its gas ` +
                `payment landed could not be determined (${errorText(
                  outcome.checkError,
                )}) — ${verifyFirst}: ${cause}`,
        );
      }
    }

    return receipt;
  }

  // Permissionless gas payment for a message that is already dispatched: the
  // rescue path when the follow-up failed, and the top-up path for a relayer
  // policy that wants more than was first paid.
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

  private async readIgpState(
    igpAddress: string,
  ): Promise<{ ok: true; data: unknown } | { ok: false; error: unknown }> {
    try {
      const state = await this.requireContractState(igpAddress);
      return { ok: true, data: state.data };
    } catch (error) {
      return { ok: false, error };
    }
  }

  // Whether a failed payForGas attempt landed anyway. It polls because the
  // transaction may still be in flight when the confirmation wait gives up, and
  // only a successful read at the end of the window can report it missing —
  // unknown is not the same as missing.
  private async awaitLandedGasPayment(
    igpAddress: string,
    messageId: string,
    minPayment: bigint,
  ): Promise<LandedGasPayment | undefined> {
    const deadline = Date.now() + payForGasSettleMs();
    let lastError: unknown;
    let readFresh = false;
    for (;;) {
      readFresh = false;
      const read = await this.readIgpState(igpAddress);
      if (read.ok) {
        // Kept outside the read's error handling: a decode failure means the
        // contract layout changed, which no retry fixes.
        const landed = findLandedGasPayment(
          readGasPayments(read.data),
          messageId,
          minPayment,
        );
        if (landed) return landed;
        readFresh = true;
      } else {
        lastError = read.error;
      }
      if (Date.now() >= deadline) break;
      await sleep(PAY_FOR_GAS_POLL_MS);
    }
    if (!readFresh) {
      throw new Error(
        `could not read IGP ${igpAddress} state to check for a landed gas payment: ${errorText(
          lastError,
        )}`,
      );
    }
    return undefined;
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

  // Fees are paid in DUST, which the NIGHT balance never reflects. Reported in
  // specks: 1 DUST = 10^15 specks.
  async getFeeTokenBalance(req: ReqGetBalance): Promise<ResFeeTokenBalance> {
    if (req.address !== this.signerAddress) {
      throw new Error(
        'DUST balance is only readable for the signer wallet address',
      );
    }
    const ctx = await this.walletContext();
    const state = await waitForSync(ctx);
    return {
      balance: state.dust?.balance(new Date()) ?? 0n,
      symbol: 'DUST',
      decimals: 15,
    };
  }

  /**
   * The ZOwnablePK owner commitment comes from the wallet's shielded
   * coinPublicKey plus the per-contract secretNonce, persisted or generated.
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
    // The agents' `index.from` must be at or before this, and nothing else in
    // the deploy flow surfaces the block.
    const deployBlock = receipts[0]?.blockHeight;
    if (deployBlock) {
      log(`${options.name} deployed at block ${deployBlock}`);
    }
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

  // Registers any unregistered NIGHT UTXOs and waits for enough DUST to cover
  // a fee. An empty wallet fails fast rather than waiting forever.
  private async ensureFeesReady(ctx: WalletContext): Promise<void> {
    const state = await waitForSync(ctx);
    const dust = state.dust?.balance(new Date()) ?? 0n;
    if (dust >= minimumDustSpecks()) return;
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

  // Join a deployed contract for proven circuit calls. Private state stored by
  // a local deploy wins over `initialPrivateState`; on a fresh machine the
  // persisted owner-state nonce restores the owner identity.
  private joinContract(
    name: MidnightContractName,
    rawContractAddress: string,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  ): Promise<any> {
    // Registry configs carry 0x-prefixed addresses, which the ledger's address
    // parser rejects.
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

// Midnight writers need capabilities beyond ISigner, like contract deploys and
// owner state. Same downcast the other protocol SDKs use.
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
