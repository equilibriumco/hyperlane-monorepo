import { expect } from 'chai';

import { ArtifactState } from '@hyperlane-xyz/provider-sdk/artifact';
import type { RawHookArtifactConfigs } from '@hyperlane-xyz/provider-sdk/hook';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { rejectionOf } from '../testing/assertions.js';
import type { SimulatedContract } from '../testing/simulated-contract.js';
import { deployIgp } from '../testing/simulated-igp.js';

import {
  MidnightIgpHookWriter,
  igpQuoteLine,
} from './hook-artifact-manager.js';

type IgpConfig = RawHookArtifactConfigs['interchainGasPaymaster'];

const SEPOLIA = 11155111;

// A signer that fails the test if the writer sends anything.
const NO_SENDS = {
  async deployMidnightContract(): Promise<never> {
    throw new Error('deployed');
  },
  async sendAndConfirmTransaction(): Promise<never> {
    throw new Error('sent');
  },
};

function igpConfig(changes: Partial<IgpConfig> = {}): IgpConfig {
  return {
    type: 'interchainGasPaymaster',
    owner: ZERO_ADDRESS_HEX_32,
    beneficiary: '',
    oracleKey: ZERO_ADDRESS_HEX_32,
    overhead: { [SEPOLIA]: 50_000 },
    oracleConfig: {
      [SEPOLIA]: { gasPrice: '1', tokenExchangeRate: '10000000000' },
    },
    ...changes,
  };
}

describe('Midnight IGP writer', () => {
  let igp: SimulatedContract;
  let writer: MidnightIgpHookWriter;

  beforeEach(async () => {
    igp = await deployIgp();
    writer = new MidnightIgpHookWriter(igp, NO_SENDS);
  });

  function deployed(config: IgpConfig) {
    return {
      artifactState: ArtifactState.DEPLOYED,
      config,
      deployed: { address: igp.address },
    } as const;
  }

  describe('update', () => {
    it('keeps the owner, and refuses any other value', async () => {
      const { owner } = (await writer.read(igp.address)).config;
      for (const kept of [owner, ZERO_ADDRESS_HEX_32]) {
        expect(
          await writer.update(deployed(igpConfig({ owner: kept }))),
        ).to.have.length(1);
      }

      const other = `0x${'77'.repeat(32)}`;
      expect(
        await rejectionOf(writer.update(deployed(igpConfig({ owner: other })))),
      ).to.include(
        `the IGP's owner cannot be changed from a config on Midnight, and the config says ${other}`,
      );
    });

    it('refuses a zero gas price or exchange rate', async () => {
      for (const oracle of [
        { gasPrice: '0', tokenExchangeRate: '10000000000' },
        { gasPrice: '1', tokenExchangeRate: '0' },
      ]) {
        expect(
          await rejectionOf(
            writer.update(
              deployed(igpConfig({ oracleConfig: { [SEPOLIA]: oracle } })),
            ),
          ),
        ).to.match(/^IGP (gasPrice|tokenExchangeRate) for 11155111 is 0/);
      }
    });
  });

  describe('create', () => {
    it('refuses an owner and a zero rate before deploying anything', async () => {
      const other = `0x${'77'.repeat(32)}`;
      expect(
        await rejectionOf(
          writer.create({ config: igpConfig({ owner: other }) }),
        ),
      ).to.include(`the config says ${other}`);
      expect(
        await rejectionOf(
          writer.create({
            config: igpConfig({
              oracleConfig: {
                [SEPOLIA]: { gasPrice: '1', tokenExchangeRate: '0' },
              },
            }),
          }),
        ),
      ).to.include('tokenExchangeRate for 11155111 is 0');
    });
  });

  it('prices a delivery the way the IGP quotes it', () => {
    // (100,000 + 50,000) x 20 gwei x 500 / 1e10 micro-NIGHT: ETH at 50,000
    // times NIGHT's price, as in the guide's example.
    expect(
      igpQuoteLine(
        SEPOLIA,
        { gasPrice: '20000000000', tokenExchangeRate: '500' },
        50_000n,
      ),
    ).to.include('a delivery of 100000 gas costs 150 NIGHT');
    expect(
      igpQuoteLine(
        SEPOLIA,
        { gasPrice: '1', tokenExchangeRate: '10000000000' },
        0n,
      ),
    ).to.include('costs 0.1 NIGHT');
  });
});
