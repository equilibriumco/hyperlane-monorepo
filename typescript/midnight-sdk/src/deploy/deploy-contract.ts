import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

import { CompiledContract } from '@midnight-ntwrk/compact-js';
import { deployContract } from '@midnight-ntwrk/midnight-js-contracts';

import {
  loadContractModule,
  witnessesFor,
  type MidnightContractName,
} from '../clients/contracts.js';
import type { MidnightTxReceipt } from '../utils/types.js';

import {
  DEFAULT_CHUNK_BUDGET_BYTES,
  NIGHT_DEPLOY_CIRCUITS,
  NIGHT_MAINTENANCE_PRIORITY,
  filteredContractClass,
  insertVerifierKeys,
  moduleFingerprintProblems,
  planChunks,
  readVerifierKeys,
  totalKeyBytes,
  verifyFullSurface,
  type ChunkPlan,
} from './chunked-deploy.js';
import type { OwnerStateStore } from './owner-state.js';
import type { ContractProviders } from './providers.js';

// The packaged artifacts hold the contract module and verifier keys only.
// Proving also needs the multi-GB prover keys and zkir circuits, which stay in
// a compiled contracts tree that `HYPERLANE_MIDNIGHT_CONTRACTS` points at.
export function artifactsPathFor(name: MidnightContractName): string {
  const compiled = process.env.HYPERLANE_MIDNIGHT_CONTRACTS;
  if (compiled) {
    return path.join(compiled, name);
  }
  return fileURLToPath(new URL(`../../artifacts/${name}`, import.meta.url));
}

export interface DeployInstanceRequest {
  name: MidnightContractName;
  args: unknown[];
  secretNonce: Uint8Array;
  providers: ContractProviders;
  // Chunked deploy for the night monolith; igp / validator-announce fit in
  // a single deploy tx under stock block limits.
  chunked: boolean;
  ownerStore: OwnerStateStore;
  log: (message: string) => void;
}

export interface DeployedInstance {
  address: string;
  receipts: MidnightTxReceipt[];
}

export async function deployContractInstance(
  req: DeployInstanceRequest,
): Promise<DeployedInstance> {
  const { name, providers, log } = req;
  const zkConfigPath = artifactsPathFor(name);
  const module = await loadContractModule(name);

  // The module comes from the packaged artifacts and the keys from
  // `zkConfigPath`; they are only the same build if the fingerprints the
  // compiler left in the module are the keys' own.
  const entries = readVerifierKeys(zkConfigPath);
  const mismatch = moduleFingerprintProblems(
    (module as { expectedVk?: Record<string, string> }).expectedVk,
    entries,
  );
  if (mismatch.length > 0) {
    throw new Error(
      `the ${name} contract module and the verifier keys at ${zkConfigPath} are not ` +
        `the same build:\n  ${mismatch.join('\n  ')}\n` +
        'Regenerate the SDK artifacts from that tree (scripts/generate-artifacts.js ' +
        'with HYPERLANE_MIDNIGHT_CONTRACTS set) and rebuild.',
    );
  }

  const chunkPlan: ChunkPlan | null = req.chunked
    ? planChunks(entries, {
        deployCircuits: NIGHT_DEPLOY_CIRCUITS,
        priority: NIGHT_MAINTENANCE_PRIORITY,
        budgetBytes: Number(
          process.env.NIGHT_CHUNK_BUDGET_BYTES ?? DEFAULT_CHUNK_BUDGET_BYTES,
        ),
      })
    : null;

  /* eslint-disable @typescript-eslint/no-explicit-any */
  const witnesses = witnessesFor(name);
  const DeployCtor = chunkPlan
    ? filteredContractClass(module.Contract as any, NIGHT_DEPLOY_CIRCUITS)
    : module.Contract;
  const compiledContract = (CompiledContract.make as any)(
    name,
    DeployCtor,
  ).pipe(
    (CompiledContract.withWitnesses as any)(witnesses),
    (CompiledContract.withCompiledFileAssets as any)(zkConfigPath),
  );

  if (chunkPlan) {
    log(
      `deploying ${name} chunked: ${chunkPlan.deploy.length} circuits in the deploy tx ` +
        `(${totalKeyBytes(chunkPlan.deploy)} B of keys) + ${chunkPlan.batches.length} ` +
        `maintenance tx(s), budget ${chunkPlan.budgetBytes} B of keys per tx`,
    );
  } else {
    log(`deploying ${name}`);
  }

  const deployed = await (deployContract as any)(providers, {
    compiledContract,
    privateStateId: `${name}-state`,
    initialPrivateState: { secretNonce: req.secretNonce },
    args: req.args,
  });
  /* eslint-enable @typescript-eslint/no-explicit-any */
  const address: string = deployed.deployTxData.public.contractAddress;
  const receipts: MidnightTxReceipt[] = [
    {
      txId: String(deployed.deployTxData.public.txId ?? ''),
      blockHeight: Number(deployed.deployTxData.public.blockHeight ?? 0),
    },
  ];
  log(`${name} deployed at ${address}`);

  // A visible, backupable copy of the maintenance-authority signing key: its
  // only other home is the level private-state DB, and losing it means no
  // verifier key can ever be added or replaced on this instance again.
  const signingKey: unknown =
    await req.providers.privateStateProvider.getSigningKey(address);
  if (signingKey != null) {
    req.ownerStore.saveMaintenanceSigningKey(
      address,
      JSON.parse(JSON.stringify(signingKey)),
    );
  } else {
    log(`WARNING: no maintenance signing key stored for ${name}`);
  }

  if (chunkPlan) {
    for (const [i, batch] of chunkPlan.batches.entries()) {
      log(
        `maintenance tx ${i + 1}/${chunkPlan.batches.length}: inserting ` +
          `${batch.length} verifier keys (${totalKeyBytes(batch)} B): ` +
          batch.map((e) => e.circuitId).join(', '),
      );
      const finalized = await insertVerifierKeys(providers, address, batch);
      receipts.push({
        txId: String(finalized.txId),
        blockHeight: Number(
          (finalized as { blockHeight?: number | bigint }).blockHeight ?? 0,
        ),
      });
      log(`tx ${finalized.txId} ${finalized.status}`);
    }
  }

  // In both modes and for every contract: a key on chain for every circuit
  // in the build, and no circuit the build does not have.
  await verifyFullSurface(providers, address, entries);
  log(`every circuit's verifier key verified on chain (${name})`);

  return { address, receipts };
}
