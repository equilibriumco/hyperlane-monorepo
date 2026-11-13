import { expect } from 'chai';

import { deployNight } from '../testing/simulated-night.js';
import { multisigOf } from '../testing/validators.js';
import { bytesToHex } from '../utils/conversion.js';

import {
  readDestinationGas,
  readRemoteRouters,
  readValidatorSetOrigins,
} from './state.js';

// Pins the positional paths against state the compiled contract built itself:
// each reader has to find what the contract's own circuits wrote and read.
describe('night state readers', () => {
  const ROUTER = new Uint8Array(32).fill(0xee);
  // A router ending in zero bytes is stored trimmed.
  const SHORT_ROUTER = new Uint8Array(32).fill(0xab, 0, 30);

  async function nightState(): Promise<unknown> {
    const night = await deployNight({
      31338: multisigOf([0, 1, 2], 2),
      31347: multisigOf([2], 1),
    });
    // 31347 has validators and no router, 31400 a router and no validators,
    // so neither list can stand in for the other.
    await night.call('enrollRemoteRouter', [31338n, ROUTER]);
    await night.call('enrollRemoteRouter', [31400n, SHORT_ROUTER]);
    await night.call('setDestinationGas', [31338n, 250_000n]);
    return (await night.requireContractState(night.address)).data;
  }

  it('lists the remote routers', async () => {
    const routers = readRemoteRouters(await nightState());
    expect(routers.sort((a, b) => a.domainId - b.domainId)).to.eql([
      { domainId: 31338, router: bytesToHex(ROUTER) },
      { domainId: 31400, router: bytesToHex(SHORT_ROUTER) },
    ]);
  });

  it('lists the origins that have a validator set', async () => {
    expect(readValidatorSetOrigins(await nightState())).to.eql([31338, 31347]);
  });

  it('lists the destination gas', async () => {
    expect(readDestinationGas(await nightState())).to.eql([
      { domainId: 31338, gas: '250000' },
    ]);
  });
});
