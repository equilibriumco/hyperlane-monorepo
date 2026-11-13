import { normalizeHexForCompare } from './conversion.js';

export function unsupportedOnMidnight(kind: string, type: string): () => never {
  return () => {
    throw new Error(`unsupported ${kind} type on Midnight: ${type}`);
  };
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
