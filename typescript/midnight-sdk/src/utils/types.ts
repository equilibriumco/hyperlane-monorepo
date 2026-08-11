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
  // Set when the payForGas follow-up landed; its own transaction id.
  payForGasTxId?: string;
  // Set instead of payForGasTxId when the follow-up's transaction was found
  // in ledger state after its confirmation failed: the payment is on chain
  // and must not be repeated, but only its row index is recoverable.
  payForGasIndex?: number;
};

export type MidnightEndpoints = {
  nodeUrl: string;
  indexerGraphqlUrl: string;
  indexerWsUrl: string;
};
