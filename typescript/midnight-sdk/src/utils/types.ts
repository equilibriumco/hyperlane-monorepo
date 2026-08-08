// Circuit calls are proven and balanced at submission time, so the
// transaction descriptor carries intent, not raw bytes. `contract` names
// the compiled artifact the signer joins to prove the call.
export type MidnightTransaction = {
  annotation?: string;
  contract: 'night' | 'igp' | 'validator-announce';
  contractAddress: string;
  circuit: string;
  args: unknown[];
  // Midnight has no dispatch-coupled hooks, so paying the relayer is a
  // second transaction needing the messageId that transferRemote returns.
  // The signer runs it as a follow-up inside the same logical submission.
  payForGas?: {
    igpAddress: string;
    destinationDomainId: number;
    gasLimit: string;
    amount: string;
  };
};

export type MidnightTxReceipt = {
  txId: string;
  blockHeight: number;
  // Set when the submission dispatched a message (transferRemote).
  messageId?: string;
  destinationDomainId?: number;
};

export type MidnightEndpoints = {
  nodeUrl: string;
  indexerGraphqlUrl: string;
  indexerWsUrl: string;
};
