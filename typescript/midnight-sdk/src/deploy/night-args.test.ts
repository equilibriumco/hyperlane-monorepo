import { expect } from 'chai';

import {
  MESSAGE_DECIMALS,
  nightConstructorArgs,
  parseMaxTransferAmount,
} from './night-args.js';

const validatorSet = {
  paddedPubkeys: [new Uint8Array(64), new Uint8Array(64)],
  count: 2n,
  threshold: 1n,
};

describe('parseMaxTransferAmount', () => {
  it('defaults to no limit', () => {
    expect(parseMaxTransferAmount(undefined)).to.equal(0n);
  });

  it('accepts a decimal string or a number', () => {
    expect(parseMaxTransferAmount('1000000')).to.equal(1_000_000n);
    expect(parseMaxTransferAmount(42)).to.equal(42n);
  });

  it('rejects fractions, negatives and hex', () => {
    for (const bad of ['1.5', '-1', '0x10', '', 'abc']) {
      expect(() => parseMaxTransferAmount(bad)).to.throw(
        'non-negative integer',
      );
    }
  });

  it('rejects a value over Uint<128>', () => {
    expect(() => parseMaxTransferAmount((1n << 128n).toString())).to.throw(
      'Uint<128>',
    );
    expect(parseMaxTransferAmount(((1n << 128n) - 1n).toString())).to.equal(
      (1n << 128n) - 1n,
    );
  });
});

describe('nightConstructorArgs', () => {
  it('lays out the nine constructor arguments in contract order', () => {
    const ownerId = new Uint8Array(32).fill(1);
    const instanceSalt = new Uint8Array(32).fill(2);

    const args = nightConstructorArgs({
      ownerId,
      instanceSalt,
      domainId: 1234n,
      validatorSet,
      localDecimals: 6n,
      maxTransferAmount: 500n,
    });

    expect(args).to.eql([
      ownerId,
      instanceSalt,
      1234n,
      validatorSet.paddedPubkeys,
      2n,
      1n,
      6n,
      MESSAGE_DECIMALS,
      500n,
    ]);
  });
});
