// The indexer is the read path for chain state; the node RPC exposes no
// query surface comparable to EVM JSON-RPC.
export class MidnightIndexerClient {
  constructor(private readonly graphqlUrl: string) {}

  async query<T>(
    query: string,
    variables?: Record<string, unknown>,
  ): Promise<T> {
    const response = await fetch(this.graphqlUrl, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ query, variables }),
    });
    if (!response.ok) {
      throw new Error(
        `Midnight indexer request failed: ${response.status} ${response.statusText}`,
      );
    }
    const body = (await response.json()) as {
      data?: T;
      errors?: { message: string }[];
    };
    if (body.errors?.length) {
      throw new Error(
        `Midnight indexer query failed: ${body.errors.map((e) => e.message).join('; ')}`,
      );
    }
    if (body.data === undefined || body.data === null) {
      throw new Error('Midnight indexer returned no data');
    }
    return body.data;
  }

  async getBlockHeight(): Promise<number> {
    const data = await this.query<{ block: { height: number } | null }>(
      `query { block { height } }`,
    );
    if (!data.block) {
      throw new Error('Midnight indexer has no blocks yet');
    }
    return data.block.height;
  }
}
