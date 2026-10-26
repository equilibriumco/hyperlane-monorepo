import type { ResolvedValidatorSet } from '../ism/validators.js';

// Wire-format precision of warp transfer amounts, meaning the max decimals in
// the route rather than the remote chain's local token decimals. Check the
// contracts repo's Scale module before changing this.
export const MESSAGE_DECIMALS = 18n;

const UINT128_MAX = (1n << 128n) - 1n;

export interface NightConstructorInputs {
  ownerId: Uint8Array;
  instanceSalt: Uint8Array;
  domainId: bigint;
  validatorSet: ResolvedValidatorSet;
  localDecimals: bigint;
  // Per-transfer limit in local NIGHT units; 0 means no limit.
  maxTransferAmount: bigint;
}

// `maxTransferAmount` arrives from the core deploy config as a decimal string
// or number; unset means no limit.
export function parseMaxTransferAmount(
  value: string | number | undefined,
): bigint {
  if (value === undefined) return 0n;
  const text = String(value);
  if (!/^\d+$/.test(text)) {
    throw new Error(
      `maxTransferAmount must be a non-negative integer in local NIGHT units, got ${JSON.stringify(value)}`,
    );
  }
  const amount = BigInt(text);
  if (amount > UINT128_MAX) {
    throw new Error(`maxTransferAmount ${text} does not fit in Uint<128>`);
  }
  return amount;
}

// The night constructor's positional arguments, in contract order.
export function nightConstructorArgs(
  inputs: NightConstructorInputs,
): unknown[] {
  return [
    inputs.ownerId,
    inputs.instanceSalt,
    inputs.domainId,
    inputs.validatorSet.paddedPubkeys,
    inputs.validatorSet.count,
    inputs.validatorSet.threshold,
    inputs.localDecimals,
    MESSAGE_DECIMALS,
    inputs.maxTransferAmount,
  ];
}
