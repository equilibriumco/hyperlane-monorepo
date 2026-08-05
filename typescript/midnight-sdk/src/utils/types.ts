// Circuit calls are proven and balanced at submission time, so the
// transaction descriptor carries intent, not raw bytes. `contract` names
// the compiled artifact the signer joins to prove the call.
export type MidnightTransaction = {
  annotation?: string;
  contract: 'night' | 'igp' | 'validator-announce';
  contractAddress: string;
  circuit: string;
  args: unknown[];
};

export type MidnightTxReceipt = {
  txId: string;
  blockHeight: number;
};

export type MidnightEndpoints = {
  nodeUrl: string;
  indexerGraphqlUrl: string;
  indexerWsUrl: string;
};
