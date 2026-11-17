import { expect } from 'chai';

import { ContractState } from '@midnight-ntwrk/compact-runtime';
import { SerializedContractState } from '@midnight-ntwrk/midnight-js-types';

import { NIGHT_DOMAIN, deployNight } from '../testing/simulated-night.js';
import { multisigOf } from '../testing/validators.js';

import { decodeContractState } from './contract-state.js';

// A state read arrives as bytes, and the compiled contract's circuits take
// only a state of their own runtime copy.
describe('decodeContractState', () => {
  it('decodes a state read into a state the circuits run against', async () => {
    const night = await deployNight({ 31338: multisigOf([0, 1], 2) });
    const { data } = await night.requireContractState(night.address);
    const onChain = new ContractState();
    onChain.data = data as ContractState['data'];

    const decoded = decodeContractState({
      contractState: SerializedContractState.fromBytes(
        'v9',
        onChain.serialize(),
      ),
    });

    expect(
      await night.runCircuit<bigint>('night', decoded.data, 'localDomain'),
    ).to.equal(BigInt(NIGHT_DOMAIN));
  });
});
