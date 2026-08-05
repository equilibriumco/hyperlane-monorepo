import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type {
  IProvider,
  ReqEstimateTransactionFee,
  ReqGetBalance,
  ReqGetBridgedSupply,
  ReqGetRemoteRouters,
  ReqGetTotalSupply,
  ReqGetToken,
  ReqIsMessageDelivered,
  ReqQuoteRemoteTransfer,
  ResEstimateTransactionFee,
  ResGetRemoteRouters,
  ResGetToken,
  ResQuoteRemoteTransfer,
} from '@hyperlane-xyz/provider-sdk/altvm';
import type { WarpArtifactConfig } from '@hyperlane-xyz/provider-sdk/warp';
import { assert } from '@hyperlane-xyz/utils';

import type { MidnightEndpoints, MidnightTransaction } from '../utils/types.js';

import { MidnightIndexerClient } from './indexer.js';

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

  /**
   * `rpcUrls` carry the node endpoint; the indexer GraphQL endpoint (the
   * read path for all chain state) travels in `gatewayUrls`.
   */
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
    return { nodeUrl, indexerGraphqlUrl };
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

  async getBalance(_req: ReqGetBalance): Promise<bigint> {
    throw new Error('MidnightProvider.getBalance: not implemented yet (#105)');
  }

  async getTotalSupply(_req: ReqGetTotalSupply): Promise<bigint> {
    throw new Error(
      'MidnightProvider.getTotalSupply: not implemented yet (#105)',
    );
  }

  async estimateTransactionFee(
    _req: ReqEstimateTransactionFee<MidnightTransaction>,
  ): Promise<ResEstimateTransactionFee> {
    throw new Error(
      'MidnightProvider.estimateTransactionFee: not implemented yet (#105)',
    );
  }

  async isMessageDelivered(_req: ReqIsMessageDelivered): Promise<boolean> {
    throw new Error(
      'MidnightProvider.isMessageDelivered: not implemented yet (#105)',
    );
  }

  async getToken(_req: ReqGetToken): Promise<ResGetToken> {
    throw new Error('MidnightProvider.getToken: not implemented yet (#105)');
  }

  async getRemoteRouters(
    _req: ReqGetRemoteRouters,
  ): Promise<ResGetRemoteRouters> {
    throw new Error(
      'MidnightProvider.getRemoteRouters: not implemented yet (#105)',
    );
  }

  async getBridgedSupply(_req: ReqGetBridgedSupply): Promise<bigint> {
    throw new Error(
      'MidnightProvider.getBridgedSupply: not implemented yet (#105)',
    );
  }

  async quoteRemoteTransfer(
    _req: ReqQuoteRemoteTransfer,
  ): Promise<ResQuoteRemoteTransfer> {
    throw new Error(
      'MidnightProvider.quoteRemoteTransfer: not implemented yet (#105)',
    );
  }

  async getMinGasForWarpDeploy(
    _warpConfig: WarpArtifactConfig,
  ): Promise<bigint> {
    throw new Error(
      'MidnightProvider.getMinGasForWarpDeploy: not implemented yet (#105)',
    );
  }
}
