import {
  createCircuitContext,
  dummyContractAddress,
  emptyZswapLocalState,
} from '@midnight-ntwrk/compact-runtime';

import { createIgpWitnesses } from '../witnesses/igp-witnesses.js';
import { createScaleWitnesses } from '../witnesses/scale-witnesses.js';

export type ContractModule = {
  Contract: new (witnesses: Record<string, unknown>) => {
    circuits: Record<
      string,
      (ctx: unknown, ...args: unknown[]) => Promise<{ result: unknown }>
    >;
  };
};

export type MidnightContractName = 'night' | 'igp' | 'validator-announce';

const moduleCache = new Map<MidnightContractName, Promise<ContractModule>>();

// Resolved relative to this file so the same path works from src (tsx) and
// dist (built package).
export function loadContractModule(
  name: MidnightContractName,
): Promise<ContractModule> {
  let cached = moduleCache.get(name);
  if (!cached) {
    const url = new URL(
      `../../artifacts/${name}/contract/index.js`,
      import.meta.url,
    );
    cached = import(url.href).then((mod) => mod as ContractModule);
    moduleCache.set(name, cached);
  }
  return cached;
}

// Read-only circuit execution never invokes the ownership witness, so an
// all-zero nonce is fine outside owner-gated calls.
export function buildNightWitnesses(
  secretNonce: Uint8Array = new Uint8Array(32),
): Record<string, unknown> {
  return {
    ...createScaleWitnesses({ secretNonce }),
    wit_secretNonce(ctx: {
      privateState: { secretNonce: Uint8Array };
    }): [{ secretNonce: Uint8Array }, Uint8Array] {
      return [ctx.privateState, ctx.privateState.secretNonce];
    },
  };
}

export function buildIgpWitnesses(
  secretNonce: Uint8Array = new Uint8Array(32),
): Record<string, unknown> {
  return {
    ...createIgpWitnesses({ secretNonce }),
    wit_secretNonce(ctx: {
      privateState: { secretNonce: Uint8Array };
    }): [{ secretNonce: Uint8Array }, Uint8Array] {
      return [ctx.privateState, ctx.privateState.secretNonce];
    },
  };
}

export function buildVaWitnesses(): Record<string, unknown> {
  return {
    wit_secretNonce(ctx: {
      privateState: { secretNonce: Uint8Array };
    }): [{ secretNonce: Uint8Array }, Uint8Array] {
      return [ctx.privateState, ctx.privateState.secretNonce];
    },
  };
}

export function witnessesFor(
  name: MidnightContractName,
): Record<string, unknown> {
  switch (name) {
    case 'night':
      return buildNightWitnesses();
    case 'igp':
      return buildIgpWitnesses();
    case 'validator-announce':
      return buildVaWitnesses();
  }
}

// Executes a circuit locally against fetched contract state — no proof
// server, no wallet, no transaction.
export async function runReadCircuit<T>(
  module: ContractModule,
  witnesses: Record<string, unknown>,
  stateData: unknown,
  circuitId: string,
  args: unknown[] = [],
): Promise<T> {
  const contract = new module.Contract(witnesses);
  const ctx = createCircuitContext(
    circuitId,
    dummyContractAddress(),
    emptyZswapLocalState({ bytes: new Uint8Array(32) }),
    stateData as never,
    { secretNonce: new Uint8Array(32) },
  );
  const circuit = contract.circuits[circuitId];
  if (!circuit) {
    throw new Error(`circuit ${circuitId} not found on contract module`);
  }
  const { result } = await circuit(ctx, ...args);
  return result as T;
}
