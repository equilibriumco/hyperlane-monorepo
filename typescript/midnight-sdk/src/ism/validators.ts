import { keccak_256 } from '@noble/hashes/sha3.js';

import {
  isArtifactDeployed,
  type Artifact,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedIsmAddress,
  IsmArtifactConfig,
  RawIsmArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/ism';

import { loadContractModule } from '../clients/contracts.js';
import { bytesToHex, hexToBytes } from '../utils/conversion.js';
import { NIGHT_ISM_SHAPE } from '../utils/errors.js';
import type { MidnightTransaction } from '../utils/types.js';

const PUBKEY_BYTES = 64;

type MultisigIsmConfig = RawIsmArtifactConfigs['messageIdMultisigIsm'];

// The slots night pads a validator set to, read from the compiled contract so
// the SDK follows the build it ships with.
export async function maxValidators(): Promise<number> {
  const night = await loadContractModule('night');
  if (!night.pureCircuits.maxValidators) {
    throw new Error(
      'the compiled night contract has no maxValidators circuit — regenerate ' +
        'the SDK artifacts from the current contracts ' +
        '(scripts/generate-artifacts.js with HYPERLANE_MIDNIGHT_CONTRACTS set)',
    );
  }
  return Number(night.pureCircuits.maxValidators());
}

// Validators are enrolled as 64-byte secp256k1 pubkeys (X || Y); the
// Hyperlane validator identity is the derived Ethereum address.
export function pubkeyToEthAddress(pubkey: Uint8Array): string {
  if (pubkey.length !== PUBKEY_BYTES) {
    throw new Error(
      `expected a 64-byte validator pubkey, got ${pubkey.length} bytes`,
    );
  }
  return bytesToHex(keccak_256(pubkey).slice(-20));
}

export interface ResolvedValidatorSet {
  // Zero-padded to the contract's slot count for its fixed-size vector.
  paddedPubkeys: Uint8Array[];
  count: bigint;
  threshold: bigint;
}

/**
 * `validators` holds the canonical eth addresses that reads and checks compare
 * against; a write additionally needs `validatorPubkeys`, each of which must
 * hash to its address. `slots` is the contract's `maxValidators()`.
 */
export function resolveValidatorSet(
  config: MultisigIsmConfig,
  slots: number,
): ResolvedValidatorSet {
  const { validators, threshold, validatorPubkeys } = config;
  if (validators.length === 0 || validators.length > slots) {
    throw new Error(
      `Midnight supports 1-${slots} validators, got ${validators.length}`,
    );
  }
  if (threshold < 1 || threshold > validators.length) {
    throw new Error(
      `threshold ${threshold} out of range for ${validators.length} validators`,
    );
  }
  // The contract counts matched registry positions, not distinct identities,
  // so a repeated validator lets one signature satisfy two slots and collapses
  // the threshold. The contract now rejects this too; fail early with a clearer
  // message. Compare case-insensitively: EIP-55 casing is not an identity.
  const loweredValidators = validators.map((v) => v.toLowerCase());
  if (new Set(loweredValidators).size !== loweredValidators.length) {
    throw new Error(
      `multisig ISM validators must be distinct, got duplicates in [${validators.join(', ')}]`,
    );
  }
  if (!validatorPubkeys || validatorPubkeys.length === 0) {
    throw new Error(
      `Midnight enrolls validators as 64-byte secp256k1 pubkeys, which cannot ` +
        `be derived from their eth addresses — add 'validatorPubkeys' (one per ` +
        `entry in 'validators', same order) to the multisig ISM config`,
    );
  }
  if (validatorPubkeys.length !== validators.length) {
    throw new Error(
      `validatorPubkeys has ${validatorPubkeys.length} entries but validators ` +
        `has ${validators.length} — they must align one-to-one`,
    );
  }

  const paddedPubkeys = validatorPubkeys.map((hex, i) => {
    const bytes = hexToBytes(hex);
    const derived = pubkeyToEthAddress(bytes);
    if (derived.toLowerCase() !== validators[i].toLowerCase()) {
      throw new Error(
        `validatorPubkeys[${i}] derives address ${derived} but validators[${i}] ` +
          `is ${validators[i]}`,
      );
    }
    return bytes;
  });
  // The contract refuses a set whose unused slots are not zero.
  while (paddedPubkeys.length < slots) {
    paddedPubkeys.push(new Uint8Array(PUBKEY_BYTES));
  }

  return {
    paddedPubkeys,
    count: BigInt(validators.length),
    threshold: BigInt(threshold),
  };
}

export function sameValidatorSet(
  current: { validators: string[]; threshold: number },
  expected: { validators: string[]; threshold: number },
): boolean {
  if (current.threshold !== expected.threshold) return false;
  const normalize = (list: string[]) =>
    [...list].map((v) => v.toLowerCase()).sort();
  const a = normalize(current.validators);
  const b = normalize(expected.validators);
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

// The owner's full replace of one origin's set.
export function setValidatorsTx(
  nightAddress: string,
  origin: number,
  set: ResolvedValidatorSet,
): MidnightTransaction {
  return {
    annotation:
      `Set validators (${set.count}) and threshold ${set.threshold} ` +
      `for origin ${origin} on ${nightAddress}`,
    contract: 'night',
    contractAddress: nightAddress,
    circuit: 'setValidatorsAndThreshold',
    args: [BigInt(origin), set.paddedPubkeys, set.count, set.threshold],
  };
}

// Each origin's multisig config out of a routing ISM's domains. Night holds
// the sets itself, so a domain cannot point at another ISM or ISM type.
export function validatorSetsByOrigin(
  domains: Record<number, Artifact<IsmArtifactConfig, DeployedIsmAddress>>,
): Map<number, MultisigIsmConfig> {
  const sets = new Map<number, MultisigIsmConfig>();
  for (const [domain, ism] of Object.entries(domains)) {
    if (
      !isArtifactDeployed(ism) ||
      ism.config.type !== 'messageIdMultisigIsm'
    ) {
      throw new Error(`origin ${domain}: ${NIGHT_ISM_SHAPE}`);
    }
    sets.set(Number(domain), ism.config);
  }
  return sets;
}
