import * as ledger from '@midnightntwrk/ledger-v9';
import { expect } from 'chai';

import {
  MAINTENANCE_THRESHOLD,
  MIN_COMMITTEE_SIZE,
  committeeFromEnv,
  formatVerifyingKey,
  parseCommittee,
} from './maintenance-committee.js';

const sampledSchnorr = () =>
  ledger.signatureVerifyingKey(ledger.sampleSigningKey()).value;
const KEY_A = sampledSchnorr();
// The compressed secp256k1 generator, a valid ecdsa key.
const KEY_B =
  '0279be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798';
const KEY_C = sampledSchnorr();

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
    expect(() => parseCommittee(`schnorr:${KEY_A},ecdsa:${KEY_B}`)).to.throw(
      /at least 3 keys/,
    );
  });

  it('refuses a key listed twice', () => {
    expect(() =>
      parseCommittee(`schnorr:${KEY_A},ecdsa:${KEY_B},schnorr:${KEY_A}`),
    ).to.throw(/listed twice/);
  });

  it('refuses an unknown kind, a missing kind and odd hex', () => {
    expect(() =>
      parseCommittee(`ed25519:${KEY_A},ecdsa:${KEY_B},schnorr:${KEY_C}`),
    ).to.throw(/expected <kind>:<hex>/);
    expect(() =>
      parseCommittee(`${KEY_A},ecdsa:${KEY_B},schnorr:${KEY_C}`),
    ).to.throw(/expected <kind>:<hex>/);
    expect(() =>
      parseCommittee(`schnorr:abc,ecdsa:${KEY_B},schnorr:${KEY_C}`),
    ).to.throw(/not a hex verifying key/);
  });

  it('refuses a key the ledger would refuse at the hand-over', () => {
    expect(() =>
      parseCommittee(
        `schnorr:${'aa'.repeat(31)},schnorr:${KEY_A},schnorr:${KEY_C}`,
      ),
    ).to.throw(
      /'schnorr:a{62}' is not a schnorr verifying key: failed to fill whole buffer/,
    );
    expect(() =>
      parseCommittee(
        `ecdsa:${'cc'.repeat(33)},schnorr:${KEY_A},schnorr:${KEY_C}`,
      ),
    ).to.throw(/is not a ecdsa verifying key/);
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
    process.env.MIDNIGHT_MAINTENANCE_COMMITTEE = `schnorr:${KEY_A},ecdsa:${KEY_B},schnorr:${KEY_C}`;
    expect(committeeFromEnv()?.map((k) => k.value)).to.deep.equal([
      KEY_A,
      KEY_B,
      KEY_C,
    ]);
  });
});
