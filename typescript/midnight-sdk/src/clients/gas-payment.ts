import type { GasPaymentRow } from './state.js';

export type LandedGasPayment = {
  index: number;
  payment: string;
};

function normalizeHex(hex: string): string {
  return (hex.startsWith('0x') ? hex.slice(2) : hex).toLowerCase();
}

// The dispatch in this same call minted the messageId, so any row carrying it
// is ours. The amount check ignores a smaller payment someone else made.
export function findLandedGasPayment(
  rows: GasPaymentRow[],
  messageId: string,
  minPayment: bigint,
): LandedGasPayment | undefined {
  const wanted = normalizeHex(messageId);
  return rows.find(
    (row) =>
      normalizeHex(row.messageId) === wanted &&
      BigInt(row.payment) >= minPayment,
  );
}

export type PayForGasOutcome =
  | { kind: 'paid'; txId: string }
  | { kind: 'recovered'; index: number }
  | {
      kind: 'failed';
      error: unknown;
      /** Neither value proves the payment will never land: a broadcast
       *  transaction stays valid far longer than the check waits. */
      absence: 'not-seen' | 'unknown';
      checkError?: unknown;
    };

export type PayForGasAttemptDeps = {
  pay: () => Promise<{ txId: string }>;
  /** Undefined means a fresh read did not see the payment. Rejects when the
   *  ledger could not be read at all. */
  findLanded: () => Promise<LandedGasPayment | undefined>;
  attempts: number;
  delayMs: number;
  sleep: (ms: number) => Promise<void>;
};

// A payment can be broadcast and still fail its confirmation wait, and the
// contract keeps every payment, so retrying blind charges twice. Retry only
// when the ledger shows the last attempt did not land.
export async function payForGasWithLandingCheck(
  deps: PayForGasAttemptDeps,
): Promise<PayForGasOutcome> {
  const attempts = Math.max(1, deps.attempts);
  let error: unknown;
  for (let attempt = 1; attempt <= attempts; attempt++) {
    if (attempt > 1) await deps.sleep(deps.delayMs);
    try {
      const { txId } = await deps.pay();
      return { kind: 'paid', txId };
    } catch (err) {
      error = err;
    }
    let landed: LandedGasPayment | undefined;
    try {
      landed = await deps.findLanded();
    } catch (checkError) {
      return { kind: 'failed', error, absence: 'unknown', checkError };
    }
    if (landed) return { kind: 'recovered', index: landed.index };
  }
  return { kind: 'failed', error, absence: 'not-seen' };
}
