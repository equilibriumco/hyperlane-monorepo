// A transfer out reads night's message nonce and a gas payment reads the IGP's
// payment count, so of two built on the same state the node takes one and
// refuses the other: on submit with error 104 when the first is already in a
// block, or by dropping it from its pool when both were waiting there. Only a
// refusal on submit is sent again. That call never left the node, while a
// dropped one may be with other nodes, and a fork can still put it in a block.
const STALE_STATE_REFUSAL = /Invalid Transaction: Custom error: 104\b/;

const MAX_CAUSE_DEPTH = 8;

// The words in the error that name the refusal, when it is one.
export function staleStateRefusal(err: unknown): string | undefined {
  let current: unknown = err;
  for (let i = 0; current instanceof Error && i < MAX_CAUSE_DEPTH; i++) {
    const found = STALE_STATE_REFUSAL.exec(current.message);
    if (found) return found[0];
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
