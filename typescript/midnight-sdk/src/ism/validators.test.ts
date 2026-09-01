import { expect } from 'chai';

import type { RawIsmArtifactConfigs } from '@hyperlane-xyz/provider-sdk/ism';

import { hexToBytes } from '../utils/conversion.js';

import {
  MAX_VALIDATORS,
  pubkeyToEthAddress,
  resolveValidatorSet,
} from './validators.js';

// The well-known anvil dev accounts #0-#2: real secp256k1 pubkeys whose
// keccak hashes derive the matching addresses.
const VALIDATORS = [
  '0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266',
  '0x70997970C51812dc3A010C7d01b50e0d17dc79C8',
  '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC',
];
const PUBKEYS = [
  '0x8318535b54105d4a7aae60c08fc45f9687181b4fdfc625bd1a753fa7397fed753547f11ca8696646f2f3acb08e31016afac23e630c5d11f59f61fef57b0d2aa5',
  '0xba5734d8f7091719471e7f7ed6b9df170dc70cc661ca05e688601ad984f068b0d67351e5f06073092499336ab0839ef8a521afd334e53807205fa2f08eec74f4',
  '0x9d9031e97dd78ff8c15aa86939de9b1e791066a0224e331bc962a2099a7b1f0464b8bbafe1535f2301c72c2cb3535b172da30b02686ab0393d348614f157fbdb',
];

type MultisigConfig = RawIsmArtifactConfigs['messageIdMultisigIsm'];

function multisigConfig(overrides: Partial<MultisigConfig>): MultisigConfig {
  return {
    type: 'messageIdMultisigIsm',
    validators: VALIDATORS,
    threshold: 2,
    validatorPubkeys: PUBKEYS,
    ...overrides,
  };
}

describe('pubkeyToEthAddress', () => {
  it('derives the eth address from a 64-byte pubkey', () => {
    for (let i = 0; i < PUBKEYS.length; i++) {
      expect(pubkeyToEthAddress(hexToBytes(PUBKEYS[i]))).to.equal(
        VALIDATORS[i].toLowerCase(),
      );
    }
  });

  it('rejects pubkeys that are not 64 bytes', () => {
    expect(() => pubkeyToEthAddress(new Uint8Array(65))).to.throw(
      '64-byte validator pubkey',
    );
  });
});

describe('resolveValidatorSet', () => {
  it('pads the pubkey vector to the contract size', () => {
    const resolved = resolveValidatorSet(multisigConfig({}));

    expect(resolved.count).to.equal(3n);
    expect(resolved.threshold).to.equal(2n);
    expect(resolved.paddedPubkeys).to.have.lengthOf(MAX_VALIDATORS);
    expect(resolved.paddedPubkeys[0]).to.eql(hexToBytes(PUBKEYS[0]));
    expect(resolved.paddedPubkeys[3]).to.eql(new Uint8Array(64));
  });

  it('requires validatorPubkeys', () => {
    expect(() =>
      resolveValidatorSet(multisigConfig({ validatorPubkeys: undefined })),
    ).to.throw('validatorPubkeys');
  });

  it('requires one pubkey per validator', () => {
    expect(() =>
      resolveValidatorSet(
        multisigConfig({ validatorPubkeys: PUBKEYS.slice(0, 2) }),
      ),
    ).to.throw('one-to-one');
  });

  it('rejects pubkeys that do not derive their validator address', () => {
    expect(() =>
      resolveValidatorSet(
        multisigConfig({
          validatorPubkeys: [PUBKEYS[1], PUBKEYS[0], PUBKEYS[2]],
        }),
      ),
    ).to.throw('derives address');
  });

  it('rejects duplicate validators', () => {
    expect(() =>
      resolveValidatorSet(
        multisigConfig({
          validators: [VALIDATORS[0], VALIDATORS[0], VALIDATORS[1]],
          validatorPubkeys: [PUBKEYS[0], PUBKEYS[0], PUBKEYS[1]],
        }),
      ),
    ).to.throw('must be distinct');
    // EIP-55 checksum casing does not make two equal addresses distinct.
    expect(() =>
      resolveValidatorSet(
        multisigConfig({
          validators: [
            VALIDATORS[0],
            VALIDATORS[0].toLowerCase(),
            VALIDATORS[1],
          ],
          validatorPubkeys: [PUBKEYS[0], PUBKEYS[0], PUBKEYS[1]],
        }),
      ),
    ).to.throw('must be distinct');
  });

  it('rejects an out-of-range threshold', () => {
    expect(() =>
      resolveValidatorSet(multisigConfig({ threshold: 4 })),
    ).to.throw('out of range');
    expect(() =>
      resolveValidatorSet(multisigConfig({ threshold: 0 })),
    ).to.throw('out of range');
  });

  it('rejects more validators than the contract vector holds', () => {
    const validators = [...VALIDATORS, ...VALIDATORS].slice(
      0,
      MAX_VALIDATORS + 1,
    );
    expect(() => resolveValidatorSet(multisigConfig({ validators }))).to.throw(
      `1-${MAX_VALIDATORS} validators`,
    );
  });
});
