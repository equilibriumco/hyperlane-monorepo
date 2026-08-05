/**
 * Minimal GraphQL client for the Midnight indexer. The indexer is the read
 * path for chain state (the node RPC exposes no query surface comparable to
 * EVM JSON-RPC); the query shapes mirror the Rust agent adapter's
 * `indexer_client.rs`.
 */
export declare class MidnightIndexerClient {
    private readonly graphqlUrl;
    constructor(graphqlUrl: string);
    query<T>(query: string, variables?: Record<string, unknown>): Promise<T>;
    getBlockHeight(): Promise<number>;
}
//# sourceMappingURL=indexer.d.ts.map