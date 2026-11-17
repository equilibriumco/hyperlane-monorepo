import { normalizeHexForCompare } from './conversion.js';

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
