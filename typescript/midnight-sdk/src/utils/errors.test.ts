import { expect } from 'chai';

import { rethrowWithCauses } from './errors.js';

describe('rethrowWithCauses', () => {
  it('puts the causes of a failure into its message', () => {
    const node = new Error('Custom error: 192');
    const wallet = new Error('SubmissionError: transaction refused', {
      cause: node,
    });
    const failure = new Error('submitTx rejected the transaction.', {
      cause: wallet,
    });

    let thrown: unknown;
    try {
      rethrowWithCauses(failure);
    } catch (err) {
      thrown = err;
    }

    expect(thrown).to.be.an.instanceOf(Error);
    expect((thrown as Error).message).to.equal(
      'submitTx rejected the transaction.: ' +
        'SubmissionError: transaction refused: Custom error: 192',
    );
    expect((thrown as Error).cause).to.equal(failure);
  });

  it('rethrows a failure with no cause as it is', () => {
    const failure = new Error('Mailbox: paused');
    expect(() => rethrowWithCauses(failure)).to.throw(failure);
    expect(() => rethrowWithCauses('not an error')).to.throw('not an error');
  });
});
