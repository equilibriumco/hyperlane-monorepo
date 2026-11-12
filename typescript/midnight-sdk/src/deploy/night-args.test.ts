import { expect } from 'chai';

import { maxValidators, resolveValidatorSet } from '../ism/validators.js';
import { SimulatedNight } from '../testing/simulated-night.js';
import { multisigOf } from '../testing/validators.js';

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
  it('lays out the ten constructor arguments in contract order', () => {
    const ownerId = new Uint8Array(32).fill(1);
    const instanceSalt = new Uint8Array(32).fill(2);

    const args = nightConstructorArgs({
      ownerId,
      instanceSalt,
      domainId: 1234n,
      initialOrigin: 31338n,
      validatorSet,
      localDecimals: 6n,
      maxTransferAmount: 500n,
    });

    expect(args).to.eql([
      ownerId,
      instanceSalt,
      1234n,
      31338n,
      validatorSet.paddedPubkeys,
      2n,
      1n,
      6n,
      MESSAGE_DECIMALS,
      500n,
    ]);
  });

  // The order above is only right if the compiled contract reads each value
  // back from where the constructor put it.
  it('is the order the compiled night constructor takes', async () => {
    const night = new SimulatedNight();
    const resolved = resolveValidatorSet(
      multisigOf([0, 1, 2], 2),
      await maxValidators(),
    );
    await night.deploy(
      nightConstructorArgs({
        ownerId: await night.ownerId(),
        instanceSalt: night.instanceSalt,
        domainId: 1234n,
        initialOrigin: 31338n,
        validatorSet: resolved,
        localDecimals: 6n,
        maxTransferAmount: 500n,
      }),
    );
    const { data } = await night.requireContractState(night.address);
    const read = <T>(circuit: string, args: unknown[] = []) =>
      night.runCircuit<T>('night', data, circuit, args);

    expect(await read('localDomain')).to.equal(1234n);
    expect(await read('localDecimals')).to.equal(6n);
    expect(await read('messageDecimals')).to.equal(MESSAGE_DECIMALS);
    expect(await read('maxTransfer')).to.equal(500n);
    expect(await read('validatorSet', [31338n])).to.eql({
      validators: resolved.paddedPubkeys,
      count: 3n,
      threshold: 2n,
    });
    // Only the constructor's origin has a set.
    expect(await read('validatorSet', [1234n])).to.include({
      count: 0n,
      threshold: 0n,
    });
  });
});
