import { keccak_256 } from '@noble/hashes/sha3.js';

import type { RawIsmArtifactConfigs } from '@hyperlane-xyz/provider-sdk/ism';

import { bytesToHex, hexToBytes } from '../utils/conversion.js';

// night.compact enrolls a fixed-size Vector<4, Bytes<64>> of validators.
export const MAX_VALIDATORS = 4;
const PUBKEY_BYTES = 64;

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
  // Zero-padded to MAX_VALIDATORS for the fixed-size contract vector.
  paddedPubkeys: Uint8Array[];
  count: bigint;
  threshold: bigint;
}

/**
 * `validators` holds the canonical eth addresses that reads and checks compare
 * against; a write additionally needs `validatorPubkeys`, each of which must
 * hash to its address.
 */
export function resolveValidatorSet(
  config: RawIsmArtifactConfigs['messageIdMultisigIsm'],
): ResolvedValidatorSet {
  const { validators, threshold, validatorPubkeys } = config;
  if (validators.length === 0 || validators.length > MAX_VALIDATORS) {
    throw new Error(
      `Midnight supports 1-${MAX_VALIDATORS} validators, got ${validators.length}`,
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
  while (paddedPubkeys.length < MAX_VALIDATORS) {
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
