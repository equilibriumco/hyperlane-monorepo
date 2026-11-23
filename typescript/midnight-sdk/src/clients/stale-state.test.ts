import { expect } from 'chai';

import {
  staleStateRefusal,
  sendWithStaleStateResend,
  type StaleStateResendDeps,
} from './stale-state.js';

const REDACTED =
  "submitTx rejected the transaction (circuit 'transferRemote'). The provider's own failure is on " +
  '`cause`, with its message redacted of anything that could carry transaction or witness material; ' +
  "read the provider's own logs for the unredacted detail.";

// Both as a local devnet gave them, flattened the way the signer rethrows them.
const REFUSED_ON_SUBMIT =
  `${REDACTED}: (FiberFailure) SubmissionError: Transaction submission error: SubmissionError: ` +
  'Transaction submission error: SubmissionError: Transaction submission failed: RpcError: 1010: ' +
  'Invalid Transaction: Custom error: 104';
const DROPPED_FROM_POOL =
  `${REDACTED}: (FiberFailure) SubmissionError: Transaction submission error: SubmissionError: ` +
  'Transaction submission error: TransactionInvalidError: Transaction is invalid and was rejected by the node';

describe('staleStateRefusal', () => {
  it('takes the refusal on submit, error 104', () => {
    expect(staleStateRefusal(new Error(REFUSED_ON_SUBMIT))).to.equal(
      'Invalid Transaction: Custom error: 104',
    );
  });

  it('takes the drop from the pool as invalid', () => {
    expect(staleStateRefusal(new Error(DROPPED_FROM_POOL))).to.equal(
      'TransactionInvalidError',
    );
  });

  it('finds the refusal in a cause', () => {
    const cause = new Error(
      'RpcError: 1010: Invalid Transaction: Custom error: 104',
    );
    expect(staleStateRefusal(new Error(REDACTED, { cause }))).to.equal(
      'Invalid Transaction: Custom error: 104',
    );
  });

  it('leaves other node errors alone', () => {
    for (const code of [196, 1040, 231]) {
      const err = new Error(
        `${REDACTED}: RpcError: 1010: Invalid Transaction: Custom error: ${code}`,
      );
      expect(staleStateRefusal(err), String(code)).to.be.undefined;
    }
  });

  // A dropped transaction can still be in another node's pool and land.
  it('leaves a transaction dropped for a full pool alone', () => {
    const err = new Error(
      `${REDACTED}: TransactionDroppedError: Transaction got dropped, the mempool likely is full and network congested`,
    );
    expect(staleStateRefusal(err)).to.be.undefined;
  });

  it('leaves failures that are not refusals alone', () => {
    expect(staleStateRefusal(new Error('Failed to prove: socket hang up'))).to
      .be.undefined;
    expect(staleStateRefusal('Custom error: 104')).to.be.undefined;
    expect(staleStateRefusal(undefined)).to.be.undefined;
  });
});

describe('sendWithStaleStateResend', () => {
  function harness(
    sends: Array<() => Promise<string>>,
    marks: Array<string | undefined>,
  ) {
    const calls = {
      send: 0,
      mark: 0,
      sleep: 0,
      refused: [] as Array<[number, string]>,
    };
    let clock = 0;
    const deps: StaleStateResendDeps<string> = {
      send: () => sends[calls.send++](),
      stateMark: async () => marks[Math.min(calls.mark++, marks.length - 1)],
      sends: 4,
      settleMs: 30,
      pollMs: 10,
      sleep: async (ms) => {
        calls.sleep++;
        clock += ms;
      },
      now: () => clock,
      onRefused: (send, refusal) => calls.refused.push([send, refusal]),
    };
    return { calls, deps };
  }

  const ok = (value: string) => async () => value;
  const fail = (message: string) => async () => {
    throw new Error(message);
  };

  it('sends once when nothing is refused', async () => {
    const { calls, deps } = harness([ok('tx')], ['a']);
    expect(await sendWithStaleStateResend(deps)).to.equal('tx');
    expect(calls.send).to.equal(1);
    expect(calls.refused).to.deep.equal([]);
  });

  it('builds again once the indexer serves a newer state', async () => {
    // Read before the send, then twice while waiting, then before the resend.
    const { calls, deps } = harness(
      [fail(REFUSED_ON_SUBMIT), ok('tx')],
      ['a', 'a', 'b', 'b'],
    );
    expect(await sendWithStaleStateResend(deps)).to.equal('tx');
    expect(calls.send).to.equal(2);
    expect(calls.refused).to.deep.equal([
      [1, 'Invalid Transaction: Custom error: 104'],
    ]);
    expect(calls.sleep).to.equal(1);
  });

  it('builds again at once when the state already moved', async () => {
    const { calls, deps } = harness(
      [fail(DROPPED_FROM_POOL), ok('tx')],
      ['a', 'b'],
    );
    expect(await sendWithStaleStateResend(deps)).to.equal('tx');
    expect(calls.sleep).to.equal(0);
  });

  it('does not send again after any other failure', async () => {
    const { calls, deps } = harness(
      [fail('Failed to prove: socket hang up'), ok('tx')],
      ['a', 'b'],
    );
    await expectRejection(sendWithStaleStateResend(deps), /Failed to prove/);
    expect(calls.send).to.equal(1);
  });

  it('stops after the last send and passes its refusal on', async () => {
    const refusals = [1, 2, 3, 4].map(() => fail(REFUSED_ON_SUBMIT));
    const { calls, deps } = harness(refusals, [
      'a',
      'b',
      'b',
      'c',
      'c',
      'd',
      'd',
    ]);
    await expectRejection(sendWithStaleStateResend(deps), /Custom error: 104$/);
    expect(calls.send).to.equal(4);
    expect(calls.refused.map(([send]) => send)).to.deep.equal([1, 2, 3]);
  });

  it('gives up when the indexer keeps serving the refused state', async () => {
    const { calls, deps } = harness([fail(REFUSED_ON_SUBMIT), ok('tx')], ['a']);
    await expectRejection(
      sendWithStaleStateResend(deps),
      /Custom error: 104 \(refused for a state that has changed, and the indexer still served the old one 0.03 s later/,
    );
    expect(calls.send).to.equal(1);
  });
});

async function expectRejection(
  promise: Promise<unknown>,
  pattern: RegExp,
): Promise<void> {
  try {
    await promise;
  } catch (err) {
    expect((err as Error).message).to.match(pattern);
    return;
  }
  expect.fail('expected a rejection');
}
