import * as path from 'node:path';
import { fileURLToPath } from 'node:url';

import { CompiledContract } from '@midnight-ntwrk/compact-js';
import { deployContract } from '@midnight-ntwrk/midnight-js-contracts';
import * as ledger from '@midnightntwrk/ledger-v9';

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
  replaceMaintenanceAuthority,
  totalKeyBytes,
  verifyFullSurface,
  type ChunkPlan,
} from './chunked-deploy.js';
import {
  MAINTENANCE_THRESHOLD,
  formatVerifyingKey,
} from './maintenance-committee.js';
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
  // The maintenance committee the deploy ends with: the holders' verifying
  // keys, or null to sample one around the deploy key.
  committee: ledger.SignatureVerifyingKey[] | null;
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

  receipts.push(await handOverMaintenance(req, address));

  return { address, receipts };
}

// The last transaction of every deploy: the deploy key, a 1-of-1 authority
// until here, hands maintenance to a committee with a threshold of two. With
// no committee configured it is the deploy key plus two sampled keys, all
// saved in owner-state.json as the backupable copy; with a configured
// committee the keys are the holders' and this host keeps none.
async function handOverMaintenance(
  req: DeployInstanceRequest,
  address: string,
): Promise<MidnightTxReceipt> {
  const { name, providers, log } = req;
  let committee: ledger.SignatureVerifyingKey[];
  let held: ledger.SigningKey[] = [];
  if (req.committee) {
    committee = req.committee;
  } else {
    const deployKey: ledger.SigningKey | null =
      await providers.privateStateProvider.getSigningKey(address);
    if (!deployKey) {
      throw new Error(
        `no maintenance signing key stored for ${name} at ${address}`,
      );
    }
    held = [deployKey, ledger.sampleSigningKey(), ledger.sampleSigningKey()];
    committee = held.map((key) => ledger.signatureVerifyingKey(key));
  }
  req.ownerStore.saveMaintenanceSigningKeys(
    address,
    held.map((key) => JSON.parse(JSON.stringify(key))),
  );

  const finalized = await replaceMaintenanceAuthority(
    providers,
    address,
    committee,
    MAINTENANCE_THRESHOLD,
  );
  log(
    `maintenance of ${name}: ${MAINTENANCE_THRESHOLD} of ${committee.length} keys ` +
      `(${held.length} held here), tx ${finalized.txId} ${finalized.status}`,
  );
  for (const key of committee) {
    log(`  ${formatVerifyingKey(key)}`);
  }

  const contractState =
    await providers.publicDataProvider.queryContractState(address);
  const authority = contractState?.maintenanceAuthority;
  const onChain: string[] = (authority?.committee ?? []).map(
    (key: { value: string }) => key.value,
  );
  if (
    authority?.threshold !== MAINTENANCE_THRESHOLD ||
    onChain.join(',') !== committee.map((key) => key.value).join(',')
  ) {
    throw new Error(
      `the maintenance authority of ${name} is not the committee just set`,
    );
  }
  return {
    txId: String(finalized.txId),
    blockHeight: Number(
      (finalized as { blockHeight?: number | bigint }).blockHeight ?? 0,
    ),
  };
}
