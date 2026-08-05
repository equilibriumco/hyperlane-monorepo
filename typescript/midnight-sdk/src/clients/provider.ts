import { getPublicStates } from '@midnight-ntwrk/midnight-js-contracts';
import { indexerPublicDataProvider } from '@midnight-ntwrk/midnight-js-indexer-public-data-provider';
import { nativeToken } from '@midnightntwrk/ledger-v9';

import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import {
  type IProvider,
  type ReqEstimateTransactionFee,
  type ReqGetBalance,
  type ReqGetBridgedSupply,
  type ReqGetRemoteRouters,
  type ReqGetToken,
  type ReqGetTotalSupply,
  type ReqIsMessageDelivered,
  type ReqQuoteRemoteTransfer,
  type ResEstimateTransactionFee,
  type ResGetRemoteRouters,
  type ResGetToken,
  type ResQuoteRemoteTransfer,
  TokenType,
} from '@hyperlane-xyz/provider-sdk/altvm';
import type { WarpArtifactConfig } from '@hyperlane-xyz/provider-sdk/warp';
import { assert } from '@hyperlane-xyz/utils';

import { bytesToHex, hexToBytes } from '../utils/conversion.js';
import type { MidnightEndpoints, MidnightTransaction } from '../utils/types.js';

import {
  buildIgpWitnesses,
  buildNightWitnesses,
  loadContractModule,
  runReadCircuit,
} from './contracts.js';
import { MidnightIndexerClient } from './indexer.js';
import { readRemoteRouters } from './state.js';

const CONTRACT_NOT_FOUND_RE =
  /not found|no contract|unknown contract|no public state/i;

// Matches the destinationGas the live warp route is configured with; callers
// override via customHookMetadata.
const DEFAULT_TRANSFER_GAS_LIMIT = 200_000n;

export class MidnightProvider implements IProvider<MidnightTransaction> {
  protected constructor(
    protected readonly metadata: ChainMetadataForAltVM,
    protected readonly endpoints: MidnightEndpoints,
    protected readonly indexer: MidnightIndexerClient,
  ) {}

  static async connect(
    metadata: ChainMetadataForAltVM,
  ): Promise<MidnightProvider> {
    const endpoints = MidnightProvider.resolveEndpoints(metadata);
    return new MidnightProvider(
      metadata,
      endpoints,
      new MidnightIndexerClient(endpoints.indexerGraphqlUrl),
    );
  }

  // rpcUrls carry the node endpoint; gatewayUrls carry the indexer GraphQL
  // endpoint, the read path for all chain state.
  protected static resolveEndpoints(
    metadata: ChainMetadataForAltVM,
  ): MidnightEndpoints {
    const [nodeUrl] = metadata.rpcUrls?.map(({ http }) => http) ?? [];
    assert(nodeUrl, `no rpcUrls in chain metadata for ${metadata.name}`);
    const [indexerGraphqlUrl] =
      metadata.gatewayUrls?.map(({ http }) => http) ?? [];
    assert(
      indexerGraphqlUrl,
      `no gatewayUrls (indexer GraphQL endpoint) in chain metadata for ${metadata.name}`,
    );
    const indexerWsUrl = `${indexerGraphqlUrl.replace(/^http/, 'ws')}/ws`;
    return { nodeUrl, indexerGraphqlUrl, indexerWsUrl };
  }

  protected async fetchContractState(
    contractAddress: string,
  ): Promise<{ data: unknown; balance: unknown } | null> {
    const publicDataProvider = indexerPublicDataProvider(
      this.endpoints.indexerGraphqlUrl,
      this.endpoints.indexerWsUrl,
    );
    try {
      const publicStates = await getPublicStates(
        publicDataProvider,
        contractAddress,
      );
      return publicStates.contractState as unknown as {
        data: unknown;
        balance: unknown;
      };
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (CONTRACT_NOT_FOUND_RE.test(message)) {
        return null;
      }
      throw err;
    }
  }

  protected async requireContractState(
    contractAddress: string,
  ): Promise<{ data: unknown; balance: unknown }> {
    const state = await this.fetchContractState(contractAddress);
    if (!state) {
      throw new Error(
        `Midnight contract ${contractAddress} not found on ${this.metadata.name}`,
      );
    }
    return state;
  }

  protected async runNightCircuit<T>(
    stateData: unknown,
    circuitId: string,
    args: unknown[] = [],
  ): Promise<T> {
    const module = await loadContractModule('night');
    return runReadCircuit<T>(
      module,
      buildNightWitnesses(),
      stateData,
      circuitId,
      args,
    );
  }

  async isHealthy(): Promise<boolean> {
    try {
      await this.indexer.getBlockHeight();
      return true;
    } catch {
      return false;
    }
  }

  getRpcUrls(): string[] {
    return this.metadata.rpcUrls?.map(({ http }) => http) ?? [];
  }

  async getHeight(): Promise<number> {
    return this.indexer.getBlockHeight();
  }

  // Contract balances live on the ContractState balance map;
  // queryUnshieldedBalances returns an empty list for contract addresses.
  // Wallet (mn_...) addresses need a synced wallet, which the read-only
  // provider does not have.
  async getBalance(req: ReqGetBalance): Promise<bigint> {
    if (req.address.startsWith('mn')) {
      throw new Error(
        'MidnightProvider.getBalance: wallet addresses require a synced wallet; ' +
          'only contract addresses are supported on the read-only provider',
      );
    }
    const state = await this.requireContractState(req.address);
    return readNativeBalance(state.balance);
  }

  // The indexer has no total-supply query for native NIGHT.
  async getTotalSupply(_req: ReqGetTotalSupply): Promise<bigint> {
    return 0n;
  }

  // Fees are paid in DUST (generated by held NIGHT, never spent from the
  // balance), so there is no native-token fee to estimate.
  async estimateTransactionFee(
    _req: ReqEstimateTransactionFee<MidnightTransaction>,
  ): Promise<ResEstimateTransactionFee> {
    return { gasUnits: 0n, gasPrice: 0, fee: 0n };
  }

  async isMessageDelivered(req: ReqIsMessageDelivered): Promise<boolean> {
    const state = await this.fetchContractState(req.mailboxAddress);
    if (!state) {
      return false;
    }
    const result = await this.runNightCircuit<unknown>(
      state.data,
      'isDelivered',
      [hexToBytes(req.messageId)],
    );
    return Boolean(result);
  }

  // The night contract is a monolith: mailbox, ISM, hook, and warp token
  // share one address. Owner is the ZOwnablePK commitment, not a wallet
  // address. Native NIGHT has no on-chain token metadata, so name/symbol
  // come from chain metadata.
  async getToken(req: ReqGetToken): Promise<ResGetToken> {
    const state = await this.requireContractState(req.tokenAddress);
    const [owner, localDecimals] = await Promise.all([
      this.runNightCircuit<Uint8Array>(state.data, 'owner'),
      this.runNightCircuit<bigint>(state.data, 'localDecimals'),
    ]);
    const native = this.metadata.nativeToken;
    return {
      address: req.tokenAddress,
      owner: bytesToHex(owner),
      tokenType: TokenType.native,
      mailboxAddress: req.tokenAddress,
      ismAddress: req.tokenAddress,
      hookAddress: req.tokenAddress,
      denom: native?.denom ?? native?.symbol ?? 'NIGHT',
      name: native?.name ?? 'NIGHT',
      symbol: native?.symbol ?? 'NIGHT',
      decimals: native?.decimals ?? Number(localDecimals),
    };
  }

  // Per-destination gas has no on-chain slot on Midnight (remote_routers is
  // domain -> router only); it lives in warp config.
  async getRemoteRouters(
    req: ReqGetRemoteRouters,
  ): Promise<ResGetRemoteRouters> {
    const state = await this.requireContractState(req.tokenAddress);
    return {
      address: req.tokenAddress,
      remoteRouters: readRemoteRouters(state.data).map(
        ({ domainId, router }) => ({
          receiverDomainId: domainId,
          receiverAddress: router,
          gas: '0',
        }),
      ),
    };
  }

  async getBridgedSupply(req: ReqGetBridgedSupply): Promise<bigint> {
    const state = await this.requireContractState(req.tokenAddress);
    return readNativeBalance(state.balance);
  }

  // The IGP is a separate contract that the token does not reference
  // on-chain, so its address must arrive as customHookAddress. An optional
  // customHookMetadata overrides the gas limit (decimal or 0x-hex).
  async quoteRemoteTransfer(
    req: ReqQuoteRemoteTransfer,
  ): Promise<ResQuoteRemoteTransfer> {
    const igpAddress = req.customHookAddress;
    if (!igpAddress) {
      throw new Error(
        'MidnightProvider.quoteRemoteTransfer: pass the IGP contract address as customHookAddress',
      );
    }
    const state = await this.requireContractState(igpAddress);
    const module = await loadContractModule('igp');
    const witnesses = buildIgpWitnesses();
    const destination = BigInt(req.destinationDomainId);
    const registered = await runReadCircuit<boolean>(
      module,
      witnesses,
      state.data,
      'isRegistered',
      [destination],
    );
    if (!registered) {
      throw new Error(
        `destination ${req.destinationDomainId} has no gas oracle on IGP ${igpAddress}`,
      );
    }
    const gasLimit = req.customHookMetadata
      ? BigInt(req.customHookMetadata)
      : DEFAULT_TRANSFER_GAS_LIMIT;
    const amount = await runReadCircuit<bigint>(
      module,
      witnesses,
      state.data,
      'quoteDispatch',
      [destination, gasLimit],
    );
    return {
      denom: this.metadata.nativeToken?.denom ?? 'NIGHT',
      amount,
    };
  }

  // Deploys cost DUST, not native NIGHT.
  async getMinGasForWarpDeploy(
    _warpConfig: WarpArtifactConfig,
  ): Promise<bigint> {
    return 0n;
  }
}

// The balance map is keyed by TokenType objects, so get() by identity cannot
// match; iterate and compare tag+raw against nativeToken() (the all-zeros
// unshielded color).
function readNativeBalance(balanceMap: unknown): bigint {
  if (!(balanceMap instanceof Map)) {
    return 0n;
  }
  const native = nativeToken() as { tag: string; raw: unknown };
  for (const [token, amount] of balanceMap as Map<
    { tag: string; raw: unknown },
    bigint | number
  >) {
    if (token?.tag === native.tag && token?.raw === native.raw) {
      return BigInt(amount);
    }
  }
  return 0n;
}
