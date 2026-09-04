import { expect } from 'chai';

import { normalizeHexForCompare } from './conversion.js';

describe('normalizeHexForCompare', () => {
  // The live brick: config carried an un-prefixed commitment while the reader
  // printed a 0x-prefixed one, so the owner guard must treat them as equal.
  const commitment = '5dbf66e2' + 'ab'.repeat(28);

  it('treats an un-prefixed and a 0x-prefixed commitment as equal', () => {
    expect(normalizeHexForCompare(commitment)).to.equal(
      normalizeHexForCompare(`0x${commitment}`),
    );
  });

  it('ignores casing differences', () => {
    expect(normalizeHexForCompare(`0x${commitment.toUpperCase()}`)).to.equal(
      normalizeHexForCompare(commitment),
    );
  });

  it('still separates genuinely different commitments', () => {
    const other = 'aaaaaaaa' + 'ab'.repeat(28);
    expect(normalizeHexForCompare(commitment)).to.not.equal(
      normalizeHexForCompare(`0x${other}`),
    );
  });
});
