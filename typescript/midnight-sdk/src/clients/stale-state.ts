// A transfer out reads night's message nonce and a gas payment reads the IGP's
// payment count, and the node checks those reads against its own state. Of two
// built on the same state, the one the node takes first wins and the other is
// refused: with error 104 when the first is already in a block, or as invalid
// when both were waiting in its pool. Either way the refused one is in no
// block, so nothing was spent.
const STALE_STATE_REFUSALS = [
  /Invalid Transaction: Custom error: 104\b/,
  /TransactionInvalidError/,
];

const MAX_CAUSE_DEPTH = 8;

// The words in the error that name the refusal, when it is one.
export function staleStateRefusal(err: unknown): string | undefined {
  let current: unknown = err;
  for (let i = 0; current instanceof Error && i < MAX_CAUSE_DEPTH; i++) {
    for (const refusal of STALE_STATE_REFUSALS) {
      const found = refusal.exec(current.message);
      if (found) return found[0];
    }
    current = current.cause;
  }
  return undefined;
}

export type StaleStateResendDeps<T> = {
  send: () => Promise<T>;
  /** Marks the contract's newest state the indexer serves. */
  stateMark: () => Promise<string | undefined>;
  /** Sends in all, the first one included. */
  sends: number;
  settleMs: number;
  pollMs: number;
  sleep: (ms: number) => Promise<void>;
  now: () => number;
  onRefused: (send: number, refusal: string) => void;
};

// A resend reads either the value the refused send read, so only one of the two
// could ever land, or a newer one, which the indexer serves only once the
// block that took the old value is final.
export async function sendWithStaleStateResend<T>(
  deps: StaleStateResendDeps<T>,
): Promise<T> {
  for (let send = 1; ; send++) {
    const mark = await deps.stateMark();
    try {
      return await deps.send();
    } catch (err) {
      const refusal = staleStateRefusal(err);
      if (send >= deps.sends || refusal === undefined) throw err;
      deps.onRefused(send, refusal);
      await awaitNewerState(mark, deps, err);
    }
  }
}

// Built again on the state it was refused for, the call would be refused again.
async function awaitNewerState<T>(
  mark: string | undefined,
  deps: StaleStateResendDeps<T>,
  refusal: unknown,
): Promise<void> {
  const deadline = deps.now() + deps.settleMs;
  while ((await deps.stateMark()) === mark) {
    if (deps.now() >= deadline) {
      const reason =
        refusal instanceof Error ? refusal.message : String(refusal);
      throw new Error(
        `${reason} (refused for a state that has changed, and the indexer ` +
          `still served the old one ${deps.settleMs / 1000} s later, so it was not sent again)`,
        { cause: refusal },
      );
    }
    await deps.sleep(deps.pollMs);
  }
}
