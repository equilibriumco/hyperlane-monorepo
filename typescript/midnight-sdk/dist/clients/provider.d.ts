import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { IProvider, ReqEstimateTransactionFee, ReqGetBalance, ReqGetBridgedSupply, ReqGetRemoteRouters, ReqGetTotalSupply, ReqGetToken, ReqIsMessageDelivered, ReqQuoteRemoteTransfer, ResEstimateTransactionFee, ResGetRemoteRouters, ResGetToken, ResQuoteRemoteTransfer } from '@hyperlane-xyz/provider-sdk/altvm';
import type { WarpArtifactConfig } from '@hyperlane-xyz/provider-sdk/warp';
import type { MidnightEndpoints, MidnightTransaction } from '../utils/types.js';
import { MidnightIndexerClient } from './indexer.js';
export declare class MidnightProvider implements IProvider<MidnightTransaction> {
    protected readonly metadata: ChainMetadataForAltVM;
    protected readonly endpoints: MidnightEndpoints;
    protected readonly indexer: MidnightIndexerClient;
    protected constructor(metadata: ChainMetadataForAltVM, endpoints: MidnightEndpoints, indexer: MidnightIndexerClient);
    static connect(metadata: ChainMetadataForAltVM): Promise<MidnightProvider>;
    /**
     * `rpcUrls` carry the node endpoint; the indexer GraphQL endpoint (the
     * read path for all chain state) travels in `gatewayUrls`.
     */
    protected static resolveEndpoints(metadata: ChainMetadataForAltVM): MidnightEndpoints;
    isHealthy(): Promise<boolean>;
    getRpcUrls(): string[];
    getHeight(): Promise<number>;
    getBalance(_req: ReqGetBalance): Promise<bigint>;
    getTotalSupply(_req: ReqGetTotalSupply): Promise<bigint>;
    estimateTransactionFee(_req: ReqEstimateTransactionFee<MidnightTransaction>): Promise<ResEstimateTransactionFee>;
    isMessageDelivered(_req: ReqIsMessageDelivered): Promise<boolean>;
    getToken(_req: ReqGetToken): Promise<ResGetToken>;
    getRemoteRouters(_req: ReqGetRemoteRouters): Promise<ResGetRemoteRouters>;
    getBridgedSupply(_req: ReqGetBridgedSupply): Promise<bigint>;
    quoteRemoteTransfer(_req: ReqQuoteRemoteTransfer): Promise<ResQuoteRemoteTransfer>;
    getMinGasForWarpDeploy(_warpConfig: WarpArtifactConfig): Promise<bigint>;
}
//# sourceMappingURL=provider.d.ts.map