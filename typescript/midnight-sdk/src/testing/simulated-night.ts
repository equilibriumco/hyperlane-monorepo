import {
  ProtocolType,
  type ChainMetadataForAltVM,
} from '@hyperlane-xyz/provider-sdk';

import { nightConstructorArgs } from '../deploy/night-args.js';
import {
  maxValidators,
  resolveValidatorSet,
  setValidatorsTx,
} from '../ism/validators.js';

import { SimulatedContract } from './simulated-contract.js';
import type { MultisigConfig } from './validators.js';

export class SimulatedNight extends SimulatedContract {
  constructor() {
    super('night');
  }
}

export const NIGHT_DOMAIN = 1234;

export const NIGHT_METADATA: ChainMetadataForAltVM = {
  name: 'midnight1',
  protocol: ProtocolType.Midnight,
  domainId: NIGHT_DOMAIN,
  chainId: NIGHT_DOMAIN,
  nativeToken: { decimals: 6, symbol: 'NIGHT', name: 'NIGHT' },
  rpcUrls: [{ http: 'http://127.0.0.1:9944' }],
  gatewayUrls: [{ http: 'http://127.0.0.1:8088/api/v3/graphql' }],
};

// A night deployed the way the SDK deploys it: the lowest origin's validators
// in the constructor, every other origin's set by the owner afterwards.
export async function deployNight(
  sets: Record<number, MultisigConfig>,
): Promise<SimulatedNight> {
  const night = new SimulatedNight();
  const slots = await maxValidators();
  const [[initialOrigin, initial], ...others] = Object.entries(sets);
  await night.deploy(
    nightConstructorArgs({
      ownerId: await night.ownerId(),
      instanceSalt: night.instanceSalt,
      domainId: BigInt(NIGHT_DOMAIN),
      initialOrigin: BigInt(initialOrigin),
      validatorSet: resolveValidatorSet(initial, slots),
      localDecimals: 6n,
      maxTransferAmount: 0n,
    }),
  );
  for (const [origin, config] of others) {
    const tx = setValidatorsTx(
      night.address,
      Number(origin),
      resolveValidatorSet(config, slots),
    );
    await night.call(tx.circuit, tx.args);
  }
  return night;
}
