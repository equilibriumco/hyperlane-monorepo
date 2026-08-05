/**
 * A transaction descriptor for Midnight. Circuit calls are proven and
 * balanced at submission time, so the descriptor carries the intent (which
 * circuit on which contract with which arguments) rather than raw bytes.
 * Satisfies the provider-sdk `AnnotatedTx` shape ({ annotation?, ...any }).
 */
export type MidnightTransaction = {
  annotation?: string;
  /** Deployed contract address the call targets. */
  contractAddress: string;
  /** Exported circuit name, e.g. 'enrollRemoteRouter'. */
  circuit: string;
  /** Circuit arguments, in declaration order. */
  args: unknown[];
};

export type MidnightTxReceipt = {
  txId: string;
  blockHeight: number;
};

/**
 * Connection endpoints for a Midnight chain. `rpcUrls` in chain metadata
 * point at the node; the indexer GraphQL endpoint (the primary read path)
 * travels in `gatewayUrls`.
 */
export type MidnightEndpoints = {
  nodeUrl: string;
  indexerGraphqlUrl: string;
};
