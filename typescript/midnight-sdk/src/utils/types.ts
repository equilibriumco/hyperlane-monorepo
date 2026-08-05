// Circuit calls are proven and balanced at submission time, so the
// transaction descriptor carries intent, not raw bytes.
export type MidnightTransaction = {
  annotation?: string;
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
