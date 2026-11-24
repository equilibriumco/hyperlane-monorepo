import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { normalizeHexForCompare } from './conversion.js';

// The chain holds an owner as a commitment that changes with every ownership
// transfer, and a transfer takes an owner id, which nothing here makes. So a
// config can only keep the owner: the zero value, or the commitment the chain
// holds now.
export function assertOwnerKept(
  contract: string,
  configured: string | undefined,
  current?: string,
): void {
  if (
    !configured ||
    normalizeHexForCompare(configured) ===
      normalizeHexForCompare(ZERO_ADDRESS_HEX_32) ||
    (current !== undefined &&
      normalizeHexForCompare(configured) === normalizeHexForCompare(current))
  ) {
    return;
  }
  throw new Error(
    `${contract}'s owner cannot be changed from a config on Midnight, and the ` +
      `config says ${configured}. The chain holds the owner as a commitment` +
      `${current === undefined ? '' : ` (${current})`}, and any other value ` +
      'would be taken as a new owner id, which most likely nobody can act ' +
      'as. Write the owner as the 32-byte zero value to keep it. To give ' +
      `${contract} to someone else, hand over the deployer's key and its ` +
      'MIDNIGHT_STATE_DIR.',
  );
}

export function unsupportedOnMidnight(kind: string, type: string): () => never {
  return () => {
    throw new Error(`unsupported ${kind} type on Midnight: ${type}`);
  };
}

// midnight-js keeps at most six links of a cause chain.
const MAX_CAUSE_DEPTH = 6;

/**
 * Rethrows a midnight-js failure with its causes in the message. The framework
 * reports a wallet's, proof server's or node's rejection with that party's own
 * message on `cause`, and callers print the message only.
 */
export function rethrowWithCauses(err: unknown): never {
  if (!(err instanceof Error) || !(err.cause instanceof Error)) throw err;
  const parts = [err.message];
  let cause: unknown = err.cause;
  for (let i = 0; cause instanceof Error && i < MAX_CAUSE_DEPTH; i++) {
    parts.push(cause.message);
    cause = cause.cause;
  }
  throw new Error(parts.join(': '), { cause: err });
}

// Night holds one validator set per origin chain, so its ISM has one shape.
export const NIGHT_ISM_SHAPE =
  "write night's ISM as a domainRoutingIsm with one messageIdMultisigIsm " +
  '(validators, validatorPubkeys, threshold) per origin chain under `domains`';

// Night is its own ISM. Any other reference means the config asked for an ISM
// night cannot take, and an apply would otherwise change nothing.
export function assertNightIsItsOwnIsm(
  nightAddress: string,
  ismAddress: string,
): void {
  if (
    normalizeHexForCompare(ismAddress) !== normalizeHexForCompare(nightAddress)
  ) {
    throw new Error(
      `night at ${nightAddress} is its own ISM and cannot use ${ismAddress}: ` +
        NIGHT_ISM_SHAPE,
    );
  }
}
