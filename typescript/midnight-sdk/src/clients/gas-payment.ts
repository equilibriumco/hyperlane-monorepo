import type { GasPaymentRow } from './state.js';

export type LandedGasPayment = {
  index: number;
  payment: string;
};

function normalizeHex(hex: string): string {
  return (hex.startsWith('0x') ? hex.slice(2) : hex).toLowerCase();
}

// Matching on messageId alone is enough for the dispatch follow-up: the id
// was minted by the dispatch in the same call, so any row carrying it is
// this call's payment. The amount check keeps a smaller third-party payment
// for the same message from passing as ours.
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
  | { kind: 'failed'; error: unknown; verified: boolean; checkError?: unknown };

export type PayForGasAttemptDeps = {
  pay: () => Promise<{ txId: string }>;
  /** Resolves this call's payment row, or undefined once ledger state has
   *  proved none landed. Rejects when the state could not be read at all. */
  findLanded: () => Promise<LandedGasPayment | undefined>;
  attempts: number;
  delayMs: number;
  sleep: (ms: number) => Promise<void>;
};

// payForGas can be broadcast and still fail while waiting for confirmation,
// and the contract appends every payment and keeps overpayment — so a blind
// retry charges the payer twice. Retry only what the ledger proves is safe
// to retry: after each failure it decides whether that attempt landed, and
// a ledger that cannot be read ends the loop instead of risking a second
// payment.
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
      return { kind: 'failed', error, verified: false, checkError };
    }
    if (landed) return { kind: 'recovered', index: landed.index };
  }
  return { kind: 'failed', error, verified: true };
}
