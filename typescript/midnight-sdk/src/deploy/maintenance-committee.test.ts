import { expect } from 'chai';

import {
  MAINTENANCE_THRESHOLD,
  MIN_COMMITTEE_SIZE,
  committeeFromEnv,
  formatVerifyingKey,
  parseCommittee,
} from './maintenance-committee.js';

// Verifying keys are hex of any even length; these are not real keys.
const KEY_A = 'ab'.repeat(32);
const KEY_B = 'cd'.repeat(32);
const KEY_C = 'ef'.repeat(32);

describe('parseCommittee', () => {
  it('reads <kind>:<hex> keys, lowercased and trimmed', () => {
    const keys = parseCommittee(
      ` schnorr:${KEY_A.toUpperCase()}, ecdsa:${KEY_B} ,schnorr:${KEY_C}`,
    );
    expect(keys).to.deep.equal([
      { tag: 'schnorr', value: KEY_A },
      { tag: 'ecdsa', value: KEY_B },
      { tag: 'schnorr', value: KEY_C },
    ]);
    expect(keys.map(formatVerifyingKey)).to.deep.equal([
      `schnorr:${KEY_A}`,
      `ecdsa:${KEY_B}`,
      `schnorr:${KEY_C}`,
    ]);
  });

  it('needs at least three keys for the threshold of two', () => {
    expect(MIN_COMMITTEE_SIZE).to.be.greaterThan(MAINTENANCE_THRESHOLD);
    expect(() => parseCommittee(`schnorr:${KEY_A},schnorr:${KEY_B}`)).to.throw(
      /at least 3 keys/,
    );
  });

  it('refuses a key listed twice', () => {
    expect(() =>
      parseCommittee(`schnorr:${KEY_A},schnorr:${KEY_B},schnorr:${KEY_A}`),
    ).to.throw(/listed twice/);
  });

  it('refuses an unknown kind, a missing kind and odd hex', () => {
    expect(() =>
      parseCommittee(`ed25519:${KEY_A},schnorr:${KEY_B},schnorr:${KEY_C}`),
    ).to.throw(/expected <kind>:<hex>/);
    expect(() =>
      parseCommittee(`${KEY_A},schnorr:${KEY_B},schnorr:${KEY_C}`),
    ).to.throw(/expected <kind>:<hex>/);
    expect(() =>
      parseCommittee(`schnorr:abc,schnorr:${KEY_B},schnorr:${KEY_C}`),
    ).to.throw(/not a hex verifying key/);
  });
});

describe('committeeFromEnv', () => {
  const saved = process.env.MIDNIGHT_MAINTENANCE_COMMITTEE;

  afterEach(() => {
    if (saved === undefined) delete process.env.MIDNIGHT_MAINTENANCE_COMMITTEE;
    else process.env.MIDNIGHT_MAINTENANCE_COMMITTEE = saved;
  });

  it('is null when the variable is unset, so the deploy samples a committee', () => {
    delete process.env.MIDNIGHT_MAINTENANCE_COMMITTEE;
    expect(committeeFromEnv()).to.equal(null);
  });

  it('is the parsed list when set', () => {
    process.env.MIDNIGHT_MAINTENANCE_COMMITTEE = `schnorr:${KEY_A},schnorr:${KEY_B},schnorr:${KEY_C}`;
    expect(committeeFromEnv()?.map((k) => k.value)).to.deep.equal([
      KEY_A,
      KEY_B,
      KEY_C,
    ]);
  });
});
