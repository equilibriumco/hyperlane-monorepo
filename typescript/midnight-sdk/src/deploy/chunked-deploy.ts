/**
 * Chunked contract deploy: deploy `night` with a minimal set of verifier keys,
 * then add the rest in follow-up maintenance transactions so every tx stays
 * under the chain's per-block `bytes_written` budget. Stagenet's stock limits
 * reject the monolithic deploy outright, and raising them needs a governance
 * motion.
 *
 * Built at the ledger level rather than through the SDK's insert helpers
 * because compact-js pins ledger operation version 'v3' and rejects newer
 * compiler output before the tx is even built.
 */
import * as fs from 'node:fs';
import * as path from 'node:path';

import { ContractState } from '@midnight-ntwrk/compact-runtime';
import {
  submitTx,
  verifyContractState,
} from '@midnight-ntwrk/midnight-js-contracts';
import { getNetworkId } from '@midnight-ntwrk/midnight-js-network-id';
import type { FinalizedTxData } from '@midnight-ntwrk/midnight-js-types';
import * as ledger from '@midnightntwrk/ledger-v9';

export interface VerifierKeyEntry {
  circuitId: string;
  key: Uint8Array;
}

export function totalKeyBytes(entries: readonly VerifierKeyEntry[]): number {
  return entries.reduce((sum, e) => sum + e.key.length, 0);
}

export function readVerifierKeys(managedDir: string): VerifierKeyEntry[] {
  const keysDir = path.join(managedDir, 'keys');
  if (!fs.existsSync(keysDir)) {
    throw new Error(
      `no verifier keys at ${keysDir} — the contract artifacts came from a ` +
        `--skip-zk compile; deploys need a full compile (rebuild the ` +
        `hyperlane-midnight contracts without SKIP_ZK, then re-run the ` +
        `midnight-sdk prebuild)`,
    );
  }
  return fs
    .readdirSync(keysDir)
    .filter((f) => f.endsWith('.verifier'))
    .sort()
    .map((f) => ({
      circuitId: f.slice(0, -'.verifier'.length),
      key: new Uint8Array(fs.readFileSync(path.join(keysDir, f))),
    }));
}

// Circuits carried by the initial deploy tx, alongside the constructor state.
export const NIGHT_DEPLOY_CIRCUITS = [
  'handle',
  'transferRemote',
  'fund',
  'enrollRemoteRouter',
] as const;

// Every remaining circuit, in insert order. The planner fails if this drifts
// from the compiled keys, so a contract surface change has to update it.
//
// The partially-populated window is inert: the constructor enrolls no remote
// routers, so `handle` and `transferRemote` reject everything until
// `enrollRemoteRouter` is called after the last maintenance tx.
export const NIGHT_MAINTENANCE_PRIORITY = [
  // admin & safety
  'pause',
  'unpause',
  'owner',
  'transferOwnership',
  'renounceOwnership',
  'setValidatorsAndThreshold',
  'unenrollRemoteRouter',
  'setDestinationGas',
  'destinationGasOf',
  // ISM / mailbox verification + state views (agent-facing)
  'verifyMultisig',
  'assertExpectedRoute',
  'isDelivered',
  'deliveryCount',
  'nonceValue',
  'messageAt',
  'vaultBalance',
  'validatorAt',
  'validatorCount',
  'thresholdValue',
  // sealed-config read views
  'localDomain',
  'isEnrolled',
  'routerOf',
  'moduleType',
  'localDecimals',
  'messageDecimals',
  'scalePower',
  'isScaleUp',
  'isPaused',
] as const;

// Per-tx budget for raw verifier-key bytes. Stagenet accepted ~25 KB
// bytesWritten and rejected ~33 KB, and the deploy tx carries another ~7 KB of
// constructor state on top of the keys.
export const DEFAULT_CHUNK_BUDGET_BYTES = 18_000;

export interface ChunkPlan {
  deploy: VerifierKeyEntry[];
  batches: VerifierKeyEntry[][];
  budgetBytes: number;
}

export interface ChunkPlanOptions {
  deployCircuits: readonly string[];
  priority: readonly string[];
  budgetBytes: number;
}

export function planChunks(
  entries: readonly VerifierKeyEntry[],
  options: ChunkPlanOptions,
): ChunkPlan {
  const { deployCircuits, priority, budgetBytes } = options;
  const byId = new Map(entries.map((e) => [e.circuitId, e]));
  if (byId.size !== entries.length) {
    throw new Error('duplicate circuit ids in verifier key entries');
  }

  const planned = [...deployCircuits, ...priority];
  const plannedSet = new Set(planned);
  if (plannedSet.size !== planned.length) {
    const dupes = planned.filter((id, i) => planned.indexOf(id) !== i);
    throw new Error(
      `circuits listed more than once in the chunk plan: ${dupes.join(', ')}`,
    );
  }
  const unknown = planned.filter((id) => !byId.has(id));
  if (unknown.length > 0) {
    throw new Error(
      `chunk plan lists circuits with no compiled verifier key: ${unknown.join(', ')}`,
    );
  }
  const unplanned = entries.filter((e) => !plannedSet.has(e.circuitId));
  if (unplanned.length > 0) {
    throw new Error(
      `compiled circuits missing from the chunk plan (update NIGHT_DEPLOY_CIRCUITS / ` +
        `NIGHT_MAINTENANCE_PRIORITY for the new contract surface): ` +
        unplanned.map((e) => e.circuitId).join(', '),
    );
  }
  const oversized = entries.filter((e) => e.key.length > budgetBytes);
  if (oversized.length > 0) {
    throw new Error(
      `verifier keys larger than the ${budgetBytes} B per-tx budget: ` +
        oversized.map((e) => `${e.circuitId} (${e.key.length} B)`).join(', '),
    );
  }

  const lookup = (id: string): VerifierKeyEntry => {
    const entry = byId.get(id);
    if (!entry) throw new Error(`missing verifier key for ${id}`);
    return entry;
  };

  const deploy = deployCircuits.map(lookup);
  const deployBytes = totalKeyBytes(deploy);
  if (deployBytes > budgetBytes) {
    throw new Error(
      `deploy circuit set carries ${deployBytes} B of verifier keys, over the ` +
        `${budgetBytes} B budget — the deploy tx also carries the constructor state`,
    );
  }

  const batches: VerifierKeyEntry[][] = [];
  let current: VerifierKeyEntry[] = [];
  let currentBytes = 0;
  for (const id of priority) {
    const entry = lookup(id);
    if (current.length > 0 && currentBytes + entry.key.length > budgetBytes) {
      batches.push(current);
      current = [];
      currentBytes = 0;
    }
    current.push(entry);
    currentBytes += entry.key.length;
  }
  if (current.length > 0) {
    batches.push(current);
  }

  return { deploy, batches, budgetBytes };
}

/**
 * Strips everything but `circuitIds` from `provableCircuits` and the initial
 * state's operation table, so the deploy tx carries no keyless operations — the
 * node rejects those with `VerifierKeyNotSet` (error 110). Call interfaces stay
 * intact, so each entry point works as soon as its key lands.
 */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
export function filteredContractClass<C extends new (...args: any[]) => any>(
  Ctor: C,
  circuitIds: readonly string[],
): C {
  const keep = new Set(circuitIds);
  return class extends Ctor {
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    constructor(...args: any[]) {
      super(...args);
      this.provableCircuits = Object.fromEntries(
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        Object.entries((this as any).provableCircuits).filter(([id]) =>
          keep.has(id),
        ),
      );
    }

    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    async initialState(...args: any[]) {
      const result = await super.initialState(...args);
      const full = result.currentContractState as ContractState;
      const filtered = new ContractState();
      filtered.data = full.data;
      filtered.maintenanceAuthority = full.maintenanceAuthority;
      filtered.balance = full.balance;
      for (const op of full.operations()) {
        const id = typeof op === 'string' ? op : Buffer.from(op).toString();
        if (keep.has(id)) {
          const operation = full.operation(op);
          if (operation) filtered.setOperation(op, operation);
        }
      }
      return { ...result, currentContractState: filtered };
    }
  };
}

// Key files carry their ledger version in an ASCII header tag, and the ledger
// API names the enclosing slot one off from that tag. Sniffing the tag rather
// than hardcoding a version means an unknown one fails loudly on a compiler
// bump instead of building a rejected tx.
const KEY_TAG_TO_OPERATION_VERSION: Record<string, 'v3' | 'v4'> = {
  'midnight:verifier-key[v6]': 'v3',
  'midnight:verifier-key[v7]': 'v4',
};

export function operationVersionForKey(key: Uint8Array): 'v3' | 'v4' {
  const header = Buffer.from(key.slice(0, 64)).toString('latin1');
  for (const [tag, version] of Object.entries(KEY_TAG_TO_OPERATION_VERSION)) {
    if (header.startsWith(`${tag}:`)) {
      return version;
    }
  }
  throw new Error(
    `unrecognized verifier key header '${header.slice(0, 40)}' — ` +
      `add its ContractOperationVersion mapping to chunked-deploy.ts`,
  );
}

/**
 * The replay counter is read from current chain state and each update bumps it,
 * so batches have to be submitted strictly in sequence.
 */
export async function insertVerifierKeys(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  providers: any,
  contractAddress: string,
  batch: readonly VerifierKeyEntry[],
): Promise<FinalizedTxData> {
  const contractState =
    await providers.publicDataProvider.queryContractState(contractAddress);
  if (!contractState) {
    throw new Error(`no contract state found at ${contractAddress}`);
  }
  const signingKey =
    await providers.privateStateProvider.getSigningKey(contractAddress);
  if (!signingKey) {
    throw new Error(
      `no maintenance signing key stored for ${contractAddress} — the deployer's ` +
        `private-state store (or owner-state.json) must hold the key sampled at deploy time`,
    );
  }

  const updates = batch.map(
    (entry) =>
      new ledger.VerifierKeyInsert(
        entry.circuitId,
        new ledger.ContractOperationVersionedVerifierKey(
          operationVersionForKey(entry.key),
          entry.key,
        ),
      ),
  );
  const counter: bigint = contractState.maintenanceAuthority.counter;
  const unsigned = new ledger.MaintenanceUpdate(
    contractAddress,
    updates,
    counter,
  );
  // The authority is 1-of-1, so one signature at committee index 0 carries it.
  const signed = unsigned.addSignature(
    0n,
    ledger.signData(signingKey, unsigned.dataToSign),
  );

  const intent = ledger.Intent.new(
    new Date(Date.now() + 60 * 60 * 1000),
  ).addMaintenanceUpdate(signed);
  // Both coin offers stay empty; the wallet's balanceTx adds the fee dust.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const unprovenTx = (ledger.Transaction as any).fromParts(
    getNetworkId(),
    undefined,
    undefined,
    intent,
  );
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  return await (submitTx as any)(providers, { unprovenTx });
}

/**
 * The same check `findDeployedContract` runs, so a contract passing here is
 * joinable by any caller holding the full artifact.
 */
export async function verifyFullSurface(
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  providers: any,
  contractAddress: string,
  entries: readonly VerifierKeyEntry[],
): Promise<void> {
  const contractState =
    await providers.publicDataProvider.queryContractState(contractAddress);
  if (!contractState) {
    throw new Error(`no contract state found at ${contractAddress}`);
  }
  verifyContractState(
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    entries.map((e) => [e.circuitId, e.key]) as any,
    contractState,
  );
}
