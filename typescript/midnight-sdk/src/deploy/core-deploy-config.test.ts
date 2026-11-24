import { expect } from 'chai';

import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { assertCoreDeployConfig } from './core-deploy-config.js';

// The shape of the CLI's example core config: chain names, not domain ids.
function coreConfig(igp: Record<string, unknown> = {}) {
  return {
    owner: ZERO_ADDRESS_HEX_32,
    defaultHook: { type: 'merkleTreeHook' },
    requiredHook: {
      type: 'interchainGasPaymaster',
      owner: ZERO_ADDRESS_HEX_32,
      overhead: { sepolia: 50_000 },
      oracleConfig: {
        sepolia: { gasPrice: '1', tokenExchangeRate: '10000000000' },
      },
      ...igp,
    },
  };
}

describe('assertCoreDeployConfig', () => {
  it('takes a config that keeps every owner', () => {
    expect(() => assertCoreDeployConfig(coreConfig())).not.to.throw();
  });

  it('refuses an owner for night or the IGP, which nobody knows before the deploy', () => {
    const owner = `0x${'77'.repeat(32)}`;
    expect(() => assertCoreDeployConfig({ ...coreConfig(), owner })).to.throw(
      "night's owner cannot be changed",
    );
    expect(() => assertCoreDeployConfig(coreConfig({ owner }))).to.throw(
      "the IGP's owner cannot be changed",
    );
  });

  it('refuses IGP values the IGP would refuse after night is deployed', () => {
    expect(() =>
      assertCoreDeployConfig(
        coreConfig({
          oracleConfig: {
            sepolia: { gasPrice: '0', tokenExchangeRate: '10000000000' },
          },
        }),
      ),
    ).to.throw('IGP gasPrice for sepolia is 0');
    expect(() =>
      assertCoreDeployConfig(coreConfig({ overhead: { holesky: 50_000 } })),
    ).to.throw(
      'no oracleConfig entry (overhead is stored with the oracle pair): holesky',
    );
  });
});
