import { expect } from 'chai';

import {
  type VerifierKeyEntry,
  operationVersionForKey,
  planChunks,
  totalKeyBytes,
} from './chunked-deploy.js';

function entry(circuitId: string, size: number): VerifierKeyEntry {
  return { circuitId, key: new Uint8Array(size) };
}

function keyWithHeader(tag: string): Uint8Array {
  return Uint8Array.from(Buffer.from(`${tag}:0000000000000000`, 'latin1'));
}

describe('planChunks', () => {
  const options = {
    deployCircuits: ['handle', 'transferRemote'],
    priority: ['a', 'b', 'c'],
    budgetBytes: 100,
  };

  it('splits the priority list into greedy budget-packed batches', () => {
    const entries = [
      entry('handle', 40),
      entry('transferRemote', 40),
      entry('a', 60),
      entry('b', 30),
      entry('c', 90),
    ];

    const plan = planChunks(entries, options);

    expect(plan.deploy.map((e) => e.circuitId)).to.eql([
      'handle',
      'transferRemote',
    ]);
    // a(60)+b(30) fit inside 100; c(90) overflows into its own batch.
    expect(plan.batches.map((batch) => batch.map((e) => e.circuitId))).to.eql([
      ['a', 'b'],
      ['c'],
    ]);
    expect(totalKeyBytes(plan.batches[0])).to.equal(90);
  });

  it('rejects compiled circuits missing from the plan', () => {
    const entries = [
      entry('handle', 10),
      entry('transferRemote', 10),
      entry('a', 10),
      entry('b', 10),
      entry('c', 10),
      entry('unexpected', 10),
    ];

    expect(() => planChunks(entries, options)).to.throw(
      'missing from the chunk plan',
    );
  });

  it('rejects planned circuits with no compiled key', () => {
    const entries = [
      entry('handle', 10),
      entry('transferRemote', 10),
      entry('a', 10),
      entry('b', 10),
    ];

    expect(() => planChunks(entries, options)).to.throw(
      'no compiled verifier key',
    );
  });

  it('rejects a single key over the per-tx budget', () => {
    const entries = [
      entry('handle', 10),
      entry('transferRemote', 10),
      entry('a', 101),
      entry('b', 10),
      entry('c', 10),
    ];

    expect(() => planChunks(entries, options)).to.throw(
      'larger than the 100 B per-tx budget',
    );
  });

  it('rejects a deploy circuit set over the budget', () => {
    const entries = [
      entry('handle', 60),
      entry('transferRemote', 60),
      entry('a', 10),
      entry('b', 10),
      entry('c', 10),
    ];

    expect(() => planChunks(entries, options)).to.throw(
      'over the 100 B budget',
    );
  });

  it('rejects duplicate circuit ids', () => {
    const entries = [
      entry('handle', 10),
      entry('handle', 10),
      entry('transferRemote', 10),
      entry('a', 10),
      entry('b', 10),
      entry('c', 10),
    ];

    expect(() => planChunks(entries, options)).to.throw(
      'duplicate circuit ids',
    );
  });
});

describe('operationVersionForKey', () => {
  it('maps known verifier key headers to operation versions', () => {
    expect(
      operationVersionForKey(keyWithHeader('midnight:verifier-key[v6]')),
    ).to.equal('v3');
    expect(
      operationVersionForKey(keyWithHeader('midnight:verifier-key[v7]')),
    ).to.equal('v4');
  });

  it('fails loudly on unknown headers', () => {
    expect(() =>
      operationVersionForKey(keyWithHeader('midnight:verifier-key[v99]')),
    ).to.throw('unrecognized verifier key header');
  });
});
