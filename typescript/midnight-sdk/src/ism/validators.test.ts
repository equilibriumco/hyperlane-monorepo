import { expect } from 'chai';

import { ArtifactState } from '@hyperlane-xyz/provider-sdk/artifact';

import { deployNight } from '../testing/simulated-night.js';
import {
  PUBKEYS,
  VALIDATORS,
  multisigOf,
  type MultisigConfig,
} from '../testing/validators.js';
import { hexToBytes } from '../utils/conversion.js';

import {
  maxValidators,
  pubkeyToEthAddress,
  resolveValidatorSet,
  sameValidatorSet,
  validatorSetsByOrigin,
} from './validators.js';

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

describe('maxValidators', () => {
  it('is the number of slots the compiled night contract stores', async () => {
    const night = await deployNight({ 31338: multisigOf([0], 1) });
    const { data } = await night.requireContractState(night.address);
    const stored = await night.runCircuit<{ validators: Uint8Array[] }>(
      'night',
      data,
      'validatorSet',
      [31338n],
    );
    expect(stored.validators).to.have.lengthOf(await maxValidators());
  });
});

describe('resolveValidatorSet', () => {
  let slots: number;

  before(async () => {
    slots = await maxValidators();
  });

  it('pads the pubkey vector to the contract size with zero keys', () => {
    const resolved = resolveValidatorSet(multisigConfig({}), slots);

    expect(resolved.count).to.equal(3n);
    expect(resolved.threshold).to.equal(2n);
    expect(resolved.paddedPubkeys).to.have.lengthOf(slots);
    expect(resolved.paddedPubkeys.slice(0, 3)).to.eql(PUBKEYS.map(hexToBytes));
    for (const unused of resolved.paddedPubkeys.slice(3)) {
      expect(unused).to.eql(new Uint8Array(64));
    }
  });

  it('requires validatorPubkeys', () => {
    expect(() =>
      resolveValidatorSet(
        multisigConfig({ validatorPubkeys: undefined }),
        slots,
      ),
    ).to.throw('validatorPubkeys');
  });

  it('requires one pubkey per validator', () => {
    expect(() =>
      resolveValidatorSet(
        multisigConfig({ validatorPubkeys: PUBKEYS.slice(0, 2) }),
        slots,
      ),
    ).to.throw('one-to-one');
  });

  it('rejects pubkeys that do not derive their validator address', () => {
    expect(() =>
      resolveValidatorSet(
        multisigConfig({
          validatorPubkeys: [PUBKEYS[1], PUBKEYS[0], PUBKEYS[2]],
        }),
        slots,
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
        slots,
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
        slots,
      ),
    ).to.throw('must be distinct');
  });

  it('rejects an out-of-range threshold', () => {
    expect(() =>
      resolveValidatorSet(multisigConfig({ threshold: 4 }), slots),
    ).to.throw('out of range');
    expect(() =>
      resolveValidatorSet(multisigConfig({ threshold: 0 }), slots),
    ).to.throw('out of range');
  });

  it('rejects more validators than the contract vector holds', () => {
    const validators = Array.from(
      { length: slots + 1 },
      (_, i) => VALIDATORS[i % VALIDATORS.length],
    );
    expect(() =>
      resolveValidatorSet(multisigConfig({ validators }), slots),
    ).to.throw(`1-${slots} validators`);
  });
});

describe('sameValidatorSet', () => {
  it('ignores validator order and address casing', () => {
    const current = multisigOf([0, 1, 2], 2);
    const expected = {
      validators: [VALIDATORS[2], VALIDATORS[0], VALIDATORS[1]].map((v) =>
        v.toLowerCase(),
      ),
      threshold: 2,
    };
    expect(sameValidatorSet(current, expected)).to.equal(true);
  });

  it('sees a changed threshold or member', () => {
    const current = multisigOf([0, 1, 2], 2);
    expect(sameValidatorSet(current, multisigOf([0, 1, 2], 3))).to.equal(false);
    expect(sameValidatorSet(current, multisigOf([0, 1], 2))).to.equal(false);
  });
});

describe('validatorSetsByOrigin', () => {
  const address = `0x${'5a'.repeat(32)}`;

  it('lists each origin with its multisig config', () => {
    const sets = validatorSetsByOrigin({
      31338: {
        artifactState: ArtifactState.DEPLOYED,
        config: multisigOf([0, 1, 2], 2),
        deployed: { address },
      },
      31347: {
        artifactState: ArtifactState.DEPLOYED,
        config: multisigOf([2], 1),
        deployed: { address },
      },
    });
    expect([...sets.keys()]).to.eql([31338, 31347]);
    expect(sets.get(31347)).to.eql(multisigOf([2], 1));
  });

  it('refuses an origin that points at another ISM address', () => {
    expect(() =>
      validatorSetsByOrigin({
        31338: {
          artifactState: ArtifactState.UNDERIVED,
          deployed: { address },
        },
      }),
    ).to.throw('origin 31338: write night');
  });

  it('refuses an origin with another ISM type', () => {
    expect(() =>
      validatorSetsByOrigin({
        31338: {
          artifactState: ArtifactState.DEPLOYED,
          config: { type: 'testIsm' },
          deployed: { address },
        },
      }),
    ).to.throw('one messageIdMultisigIsm');
  });
});
