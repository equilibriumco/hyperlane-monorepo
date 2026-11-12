import {
  createCircuitContext,
  createConstructorContext,
  dummyContractAddress,
  emptyZswapLocalState,
  type ChargedState,
} from '@midnight-ntwrk/compact-runtime';

import {
  ProtocolType,
  type ChainMetadataForAltVM,
} from '@hyperlane-xyz/provider-sdk';

import { loadContractModule, witnessesFor } from '../clients/contracts.js';
import {
  MidnightReadClient,
  type FetchedContractState,
} from '../clients/read-client.js';
import { nightConstructorArgs } from '../deploy/night-args.js';
import { userPkEither } from '../deploy/owner-state.js';
import {
  maxValidators,
  resolveValidatorSet,
  setValidatorsTx,
} from '../ism/validators.js';

import type { MultisigConfig } from './validators.js';

const COIN_PUBLIC_KEY = { bytes: new Uint8Array(32).fill(9) };
const PRIVATE_STATE = { secretNonce: new Uint8Array(32).fill(7) };

/**
 * A night contract held in memory for unit tests. The compiled contract's own
 * constructor and circuits run against real ledger state, as the owner, with
 * no node, indexer or proof; reads go through the usual client surface.
 */
export class SimulatedNight extends MidnightReadClient {
  readonly address = `0x${'5a'.repeat(32)}`;
  readonly instanceSalt = new Uint8Array(32).fill(3);
  private state: ChargedState | undefined;

  constructor() {
    super('midnight1', {
      nodeUrl: '',
      indexerGraphqlUrl: '',
      indexerWsUrl: '',
    });
  }

  async ownerId(): Promise<Uint8Array> {
    const night = await loadContractModule('night');
    return night.pureCircuits.computeOwnerId(
      userPkEither(COIN_PUBLIC_KEY.bytes),
      PRIVATE_STATE.secretNonce,
    );
  }

  // Runs the constructor on its positional arguments.
  async deploy(constructorArgs: unknown[]): Promise<void> {
    const night = await loadContractModule('night');
    const { currentContractState } = await new night.Contract(
      witnessesFor('night'),
    ).initialState(
      createConstructorContext(PRIVATE_STATE, COIN_PUBLIC_KEY),
      ...constructorArgs,
    );
    this.state = currentContractState.data;
  }

  // Runs a circuit as the owner and keeps the state it leaves behind.
  async call(circuit: string, args: unknown[]): Promise<void> {
    if (!this.state) {
      throw new Error('the simulated night is not deployed yet');
    }
    const night = await loadContractModule('night');
    const run = new night.Contract(witnessesFor('night')).circuits[circuit];
    if (!run) {
      throw new Error(`circuit ${circuit} not found on the night contract`);
    }
    const { context } = await run(
      createCircuitContext(
        circuit,
        dummyContractAddress(),
        emptyZswapLocalState(COIN_PUBLIC_KEY),
        this.state,
        PRIVATE_STATE,
      ),
      ...args,
    );
    this.state = context.queryContexts[dummyContractAddress()].state;
  }

  override async fetchContractState(): Promise<FetchedContractState | null> {
    return this.state ? { data: this.state, balance: 0n } : null;
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
