import {
  createCircuitContext,
  createConstructorContext,
  dummyContractAddress,
  emptyZswapLocalState,
  type ChargedState,
} from '@midnight-ntwrk/compact-runtime';

import {
  loadContractModule,
  witnessesFor,
  type MidnightContractName,
} from '../clients/contracts.js';
import {
  MidnightReadClient,
  type FetchedContractState,
} from '../clients/read-client.js';
import { userPkEither } from '../deploy/owner-state.js';

const COIN_PUBLIC_KEY = { bytes: new Uint8Array(32).fill(9) };
const PRIVATE_STATE = { secretNonce: new Uint8Array(32).fill(7) };

/**
 * A contract held in memory for unit tests. The compiled contract's own
 * constructor and circuits run against real ledger state, as the owner, with
 * no node, indexer or proof; reads go through the usual client surface.
 */
export class SimulatedContract extends MidnightReadClient {
  readonly address = `0x${'5a'.repeat(32)}`;
  readonly instanceSalt = new Uint8Array(32).fill(3);
  private state: ChargedState | undefined;

  constructor(private readonly contractName: MidnightContractName) {
    super('midnight1', {
      nodeUrl: '',
      indexerGraphqlUrl: '',
      indexerWsUrl: '',
    });
  }

  async ownerId(): Promise<Uint8Array> {
    const module = await loadContractModule(this.contractName);
    return module.pureCircuits.computeOwnerId(
      userPkEither(COIN_PUBLIC_KEY.bytes),
      PRIVATE_STATE.secretNonce,
    );
  }

  // Runs the constructor on its positional arguments.
  async deploy(constructorArgs: unknown[]): Promise<void> {
    const module = await loadContractModule(this.contractName);
    const { currentContractState } = await new module.Contract(
      witnessesFor(this.contractName),
    ).initialState(
      createConstructorContext(PRIVATE_STATE, COIN_PUBLIC_KEY),
      ...constructorArgs,
    );
    this.state = currentContractState.data;
  }

  // Runs a circuit as the owner and keeps the state it leaves behind.
  async call(circuit: string, args: unknown[]): Promise<void> {
    if (!this.state) {
      throw new Error(`the simulated ${this.contractName} is not deployed yet`);
    }
    const module = await loadContractModule(this.contractName);
    const run = new module.Contract(witnessesFor(this.contractName)).circuits[
      circuit
    ];
    if (!run) {
      throw new Error(
        `circuit ${circuit} not found on the ${this.contractName} contract`,
      );
    }
    const { context } = await run(
      createCircuitContext({
        circuitId: circuit,
        contractAddress: dummyContractAddress(),
        coinPublicKeyOrZswapState: emptyZswapLocalState(COIN_PUBLIC_KEY),
        contractState: this.state,
        privateState: PRIVATE_STATE,
      }),
      ...args,
    );
    this.state = context.queryContexts[dummyContractAddress()].state;
  }

  override async fetchContractState(): Promise<FetchedContractState | null> {
    return this.state ? { data: this.state, balance: 0n } : null;
  }
}
