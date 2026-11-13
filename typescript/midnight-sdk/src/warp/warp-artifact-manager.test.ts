import { expect } from 'chai';

import {
  ArtifactState,
  type ArtifactDeployed,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedWarpAddress,
  RawWarpArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/warp';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { rejectionOf } from '../testing/assertions.js';
import {
  NIGHT_METADATA,
  deployNight,
  type SimulatedNight,
} from '../testing/simulated-night.js';
import { multisigOf } from '../testing/validators.js';
import { bytesToHex } from '../utils/conversion.js';

import { MidnightNativeWarpWriter } from './warp-artifact-manager.js';

type NativeWarpArtifact = ArtifactDeployed<
  RawWarpArtifactConfigs['native'],
  DeployedWarpAddress
>;

const ANVIL2 = 31338;
const ROUTER = new Uint8Array(32).fill(0xee);

describe('Midnight native warp writer', () => {
  let night: SimulatedNight;
  let writer: MidnightNativeWarpWriter;

  beforeEach(async () => {
    night = await deployNight({ [ANVIL2]: multisigOf([0, 1, 2], 2) });
    await night.call('enrollRemoteRouter', [BigInt(ANVIL2), ROUTER]);
    await night.call('setDestinationGas', [BigInt(ANVIL2), 250_000n]);
    writer = new MidnightNativeWarpWriter(night, NIGHT_METADATA);
  });

  // The route as it reads from the chain, with the ISM the config names.
  async function routeWithIsm(
    ismAddress?: string,
  ): Promise<NativeWarpArtifact> {
    const current = await writer.read(night.address);
    return {
      ...current,
      config: {
        ...current.config,
        interchainSecurityModule:
          ismAddress === undefined
            ? undefined
            : {
                artifactState: ArtifactState.UNDERIVED,
                deployed: { address: ismAddress },
              },
      },
    };
  }

  it('reads the enrolled routers and their destination gas', async () => {
    const { config } = await writer.read(night.address);
    expect(config.remoteRouters).to.eql({
      [ANVIL2]: { address: bytesToHex(ROUTER) },
    });
    expect(config.destinationGas).to.eql({ [ANVIL2]: '250000' });
  });

  it('takes no ISM, or night itself as the ISM', async () => {
    expect(await writer.update(await routeWithIsm())).to.eql([]);
    expect(await writer.update(await routeWithIsm(night.address))).to.eql([]);
  });

  // A route config with the ISM as one top-level multisig makes the generic
  // flow "deploy a new ISM", which on Midnight is only the zero-address
  // sentinel.
  it('refuses the sentinel a stale ISM shape ends up as', async () => {
    expect(
      await rejectionOf(writer.update(await routeWithIsm(ZERO_ADDRESS_HEX_32))),
    ).to.include("write night's ISM as a domainRoutingIsm");
  });
});
