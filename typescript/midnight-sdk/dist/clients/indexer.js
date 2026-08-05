/**
 * Minimal GraphQL client for the Midnight indexer. The indexer is the read
 * path for chain state (the node RPC exposes no query surface comparable to
 * EVM JSON-RPC); the query shapes mirror the Rust agent adapter's
 * `indexer_client.rs`.
 */
export class MidnightIndexerClient {
    graphqlUrl;
    constructor(graphqlUrl) {
        this.graphqlUrl = graphqlUrl;
    }
    async query(query, variables) {
        const response = await fetch(this.graphqlUrl, {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({ query, variables }),
        });
        if (!response.ok) {
            throw new Error(`Midnight indexer request failed: ${response.status} ${response.statusText}`);
        }
        const body = (await response.json());
        if (body.errors?.length) {
            throw new Error(`Midnight indexer query failed: ${body.errors.map((e) => e.message).join('; ')}`);
        }
        if (body.data === undefined || body.data === null) {
            throw new Error('Midnight indexer returned no data');
        }
        return body.data;
    }
    async getBlockHeight() {
        const data = await this.query(`query { block { height } }`);
        if (!data.block) {
            throw new Error('Midnight indexer has no blocks yet');
        }
        return data.block.height;
    }
}
//# sourceMappingURL=indexer.js.map