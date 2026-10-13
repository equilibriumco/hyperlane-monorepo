import { createHash } from 'node:crypto';

import { expect } from 'chai';

import {
  type VerifierKeyEntry,
  compareVerifierKeys,
  describeVerifierKeyDrift,
  moduleFingerprintProblems,
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

describe('compareVerifierKeys', () => {
  const key = (byte: number) => Uint8Array.from([byte, byte, byte]);
  const build: VerifierKeyEntry[] = [
    { circuitId: 'handle', key: key(1) },
    { circuitId: 'fund', key: key(2) },
    { circuitId: 'pause', key: key(3) },
  ];

  it('reports nothing when the chain holds exactly the build', () => {
    const onChain = new Map([
      ['pause', key(3)],
      ['handle', key(1)],
      ['fund', key(2)],
    ]);
    const drift = compareVerifierKeys(build, onChain);
    expect(drift).to.eql({ missing: [], extra: [], wrong: [] });
    expect(describeVerifierKeyDrift(drift)).to.equal(null);
  });

  it('names every missing, extra and wrong circuit', () => {
    const onChain = new Map([
      ['handle', key(1)],
      ['pause', key(9)],
      ['ghost', key(4)],
    ]);
    const drift = compareVerifierKeys(build, onChain);
    expect(drift).to.eql({
      missing: ['fund'],
      extra: ['ghost'],
      wrong: ['pause'],
    });
    expect(describeVerifierKeyDrift(drift)).to.equal(
      'missing circuit: fund; extra circuit: ghost; wrong key: pause',
    );
  });
});

describe('moduleFingerprintProblems', () => {
  const sha256 = (bytes: Uint8Array) =>
    createHash('sha256').update(bytes).digest('hex');
  const keys: VerifierKeyEntry[] = [
    { circuitId: 'handle', key: Uint8Array.from([1, 2, 3]) },
    { circuitId: 'fund', key: Uint8Array.from([4, 5, 6]) },
  ];
  const ownFingerprints = Object.fromEntries(
    keys.map((e) => [e.circuitId, sha256(e.key)]),
  );

  it('accepts a module compiled with the keys being deployed', () => {
    expect(moduleFingerprintProblems(ownFingerprints, keys)).to.eql([]);
  });

  it('names a key from another build, a missing key and an unknown circuit', () => {
    const otherBuild = {
      handle: sha256(Uint8Array.from([9, 9, 9])),
      fund: ownFingerprints.fund,
      pause: sha256(Uint8Array.from([7])),
    };
    const problems = moduleFingerprintProblems(otherBuild, [
      ...keys,
      { circuitId: 'ghost', key: Uint8Array.from([8]) },
    ]);
    expect(problems.map((p) => p.split(':')[0])).to.eql([
      'handle',
      'ghost',
      'pause',
    ]);
  });

  it('refuses a module built without keys', () => {
    expect(moduleFingerprintProblems(undefined, keys)).to.have.length(1);
  });
});
