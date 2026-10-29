import { expect } from 'chai';

import { privateStatePassword } from './private-state-password.js';

describe('privateStatePassword', () => {
  const saved = process.env.MIDNIGHT_STATE_PASSWORD;

  afterEach(() => {
    if (saved === undefined) delete process.env.MIDNIGHT_STATE_PASSWORD;
    else process.env.MIDNIGHT_STATE_PASSWORD = saved;
  });

  it('is required off a local devnet', () => {
    delete process.env.MIDNIGHT_STATE_PASSWORD;
    expect(() => privateStatePassword('stagenet')).to.throw(
      /MIDNIGHT_STATE_PASSWORD is required on stagenet/,
    );
  });

  it('is the one set, on any network', () => {
    process.env.MIDNIGHT_STATE_PASSWORD = 'from-the-operator';
    expect(privateStatePassword('stagenet')).to.equal('from-the-operator');
    expect(privateStatePassword('undeployed')).to.equal('from-the-operator');
  });

  it('falls back to the devnet one on a local devnet', () => {
    delete process.env.MIDNIGHT_STATE_PASSWORD;
    expect(privateStatePassword('undeployed')).to.be.a('string').and.not.be
      .empty;
  });
});
