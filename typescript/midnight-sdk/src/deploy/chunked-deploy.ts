/**
 * Chunked contract deploy, ported from hyperlane-midnight (#94): deploy
 * `night` with a minimal set of verifier keys, then add the remaining keys
 * in follow-up maintenance transactions so every tx stays under the chain's
 * per-block `bytes_written` budget. Stagenet's stock limits reject the
 * monolithic 30-circuit deploy (~51 KB of verifier keys; empirical
 * acceptance ceiling ~25 KB bytesWritten per tx), and raising them needs a
 * chain governance motion — chunking needs none.
 *
 * Why this is hand-built at the ledger level instead of the SDK's
 * `addOrReplaceContractOperation` / `submitInsertVerifierKeyTx`:
 * compact-js 2.5.5-rc.6 hardcodes ledger operation version 'v3'
 * (`verifier-key[v6]`), which rejects compactc 0.33.0-rc.2 output
 * (`verifier-key[v7]` = version 'v4') before the tx is ever built.
 * Everything else — providers, wallet fee balancing, submission,
 * finalization watching — reuses the stock SDK via `submitTx`.
 * Maintenance updates carry no ZK proof; they are authorized purely by
 * the contract maintenance authority's signature.
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

/** Read every `keys/<circuit>.verifier` under a contract's artifacts dir. */
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

// Circuits carried by the initial deploy tx itself (alongside the
// constructor state): the bridge essentials, per the #94 scope.
export const NIGHT_DEPLOY_CIRCUITS = [
  'handle',
  'transferRemote',
  'fund',
  'enrollRemoteRouter',
] as const;

// Every remaining exported circuit, in maintenance-insert priority order:
// operator safety/admin first, then the verification/state surface the
// agents read, then the sealed-config read views. The planner hard-fails
// if this list and the compiled keys ever drift apart, so a night.compact
// surface change must update it.
//
// Deploy-order safety: the constructor enrolls NO remote routers, so
// `handle` / `transferRemote` reject everything until `enrollRemoteRouter`
// is CALLED — enrollment comes after the last maintenance tx, making the
// partially-populated window inert.
export const NIGHT_MAINTENANCE_PRIORITY = [
  // admin & safety
  'pause',
  'unpause',
  'owner',
  'transferOwnership',
  'renounceOwnership',
  'setValidatorsAndThreshold',
  'unenrollRemoteRouter',
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

// Default per-tx budget for RAW verifier-key bytes. Live stagenet
// measurements (2026-07-16, node 2.0.0-rc.4 defaults): 24,885 bytesWritten
// accepted, 33,522 rejected. The deploy tx adds ~7 KB of constructor state
// and a maintenance tx only ~100 B of structure, so 18 KB of keys keeps
// every tx comfortably under the observed ceiling.
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

/**
 * Split a contract's verifier keys into the deploy set plus greedy
 * budget-packed maintenance batches that preserve `priority` order.
 * Fails loudly on any drift between the compiled keys and the plan
 * inputs, or on a single key exceeding the budget.
 */
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
 * Subclass a generated contract constructor so only `circuitIds` remain in
 * `provableCircuits` AND in the initial contract state's operation table.
 * `deployContract` then fetches/embeds verifier keys for just that subset,
 * and the deploy tx carries no keyless operations — the node rejects those
 * with `MalformedError::VerifierKeyNotSet` (custom error 110). The other
 * entry points don't exist on-chain until a maintenance tx inserts them.
 * `circuits` / `impureCircuits` stay intact — call interfaces work as soon
 * as the matching key lands.
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

// compactc's key files carry their ledger version in an ASCII header tag;
// the ledger API names the enclosing ContractOperation slot one off from
// the tag (verifier-key[v7] lives in the 'v4' slot). Sniffing the tag
// (instead of hardcoding like compact-js does) keeps the builder honest
// across compiler bumps — an unknown tag fails loudly.
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
 * Build, sign, and submit ONE maintenance tx inserting the batch's
 * verifier keys. Reads the maintenance-authority replay counter from the
 * CURRENT on-chain state, so batches must be submitted strictly
 * sequentially (each update bumps the counter by one).
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
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    contractAddress as any,
    updates,
    counter,
  );
  // deployContract creates a 1-of-1 authority from the sampled signing
  // key, so a single signature at committee index 0 carries the update.
  const signed = unsigned.addSignature(
    0n,
    ledger.signData(signingKey, unsigned.dataToSign),
  );

  const intent = ledger.Intent.new(
    new Date(Date.now() + 60 * 60 * 1000),
  ).addMaintenanceUpdate(signed);
  // Maintenance updates sit in the fallible segment; guaranteed/fallible
  // coin offers stay empty and the wallet's balanceTx adds the fee dust.
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
 * Assert every compiled verifier key is present and byte-identical in the
 * CURRENT on-chain contract state. Mirrors the SDK check that
 * `findDeployedContract` runs, so a contract passing here is joinable by
 * any caller with the full compiled artifact.
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
