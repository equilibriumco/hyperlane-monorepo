import { expect } from 'chai';

import { assertOwnerKept, rethrowWithCauses } from './errors.js';

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

describe('assertOwnerKept', () => {
  const current = `0x${'ab'.repeat(32)}`;

  it('takes the zero value, and the commitment the chain holds in any case', () => {
    for (const owner of [
      undefined,
      `0x${'00'.repeat(32)}`,
      '00'.repeat(32),
      current,
      current.slice(2).toUpperCase(),
    ]) {
      expect(() => assertOwnerKept('night', owner, current)).not.to.throw();
    }
  });

  it('refuses any other value, as there is nothing to compare it with before a deploy', () => {
    const other = `0x${'77'.repeat(32)}`;
    expect(() => assertOwnerKept('night', other, current)).to.throw(
      `the config says ${other}. The chain holds the owner as a commitment (${current})`,
    );
    expect(() => assertOwnerKept('the IGP', current)).to.throw(
      "the IGP's owner cannot be changed",
    );
    expect(() =>
      assertOwnerKept('night', `0x${'00'.repeat(20)}`, current),
    ).to.throw('Write the owner as the 32-byte zero value');
  });
});
