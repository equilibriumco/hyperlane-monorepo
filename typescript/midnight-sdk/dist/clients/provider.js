import { assert } from '@hyperlane-xyz/utils';
import { MidnightIndexerClient } from './indexer.js';
export class MidnightProvider {
    metadata;
    endpoints;
    indexer;
    constructor(metadata, endpoints, indexer) {
        this.metadata = metadata;
        this.endpoints = endpoints;
        this.indexer = indexer;
    }
    static async connect(metadata) {
        const endpoints = MidnightProvider.resolveEndpoints(metadata);
        return new MidnightProvider(metadata, endpoints, new MidnightIndexerClient(endpoints.indexerGraphqlUrl));
    }
    /**
     * `rpcUrls` carry the node endpoint; the indexer GraphQL endpoint (the
     * read path for all chain state) travels in `gatewayUrls`.
     */
    static resolveEndpoints(metadata) {
        const [nodeUrl] = metadata.rpcUrls?.map(({ http }) => http) ?? [];
        assert(nodeUrl, `no rpcUrls in chain metadata for ${metadata.name}`);
        const [indexerGraphqlUrl] = metadata.gatewayUrls?.map(({ http }) => http) ?? [];
        assert(indexerGraphqlUrl, `no gatewayUrls (indexer GraphQL endpoint) in chain metadata for ${metadata.name}`);
        return { nodeUrl, indexerGraphqlUrl };
    }
    async isHealthy() {
        try {
            await this.indexer.getBlockHeight();
            return true;
        }
        catch {
            return false;
        }
    }
    getRpcUrls() {
        return this.metadata.rpcUrls?.map(({ http }) => http) ?? [];
    }
    async getHeight() {
        return this.indexer.getBlockHeight();
    }
    async getBalance(_req) {
        throw new Error('MidnightProvider.getBalance: not implemented yet (#105)');
    }
    async getTotalSupply(_req) {
        throw new Error('MidnightProvider.getTotalSupply: not implemented yet (#105)');
    }
    async estimateTransactionFee(_req) {
        throw new Error('MidnightProvider.estimateTransactionFee: not implemented yet (#105)');
    }
    async isMessageDelivered(_req) {
        throw new Error('MidnightProvider.isMessageDelivered: not implemented yet (#105)');
    }
    async getToken(_req) {
        throw new Error('MidnightProvider.getToken: not implemented yet (#105)');
    }
    async getRemoteRouters(_req) {
        throw new Error('MidnightProvider.getRemoteRouters: not implemented yet (#105)');
    }
    async getBridgedSupply(_req) {
        throw new Error('MidnightProvider.getBridgedSupply: not implemented yet (#105)');
    }
    async quoteRemoteTransfer(_req) {
        throw new Error('MidnightProvider.quoteRemoteTransfer: not implemented yet (#105)');
    }
    async getMinGasForWarpDeploy(_warpConfig) {
        throw new Error('MidnightProvider.getMinGasForWarpDeploy: not implemented yet (#105)');
    }
}
//# sourceMappingURL=provider.js.map