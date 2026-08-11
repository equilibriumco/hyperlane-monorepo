// Permanent off-chain implementation of power-of-10 division (Compact has no
// integer division); the circuit binds each result in Field arithmetic.
// mulPow10 is native in-circuit since the contracts repo's typed-vectors
// refactor, so no multiplication witness exists anymore. Mirrors
// contracts/src/witnesses/scale-witnesses.ts in the hyperlane-midnight
// repo — keep in sync.
export function createScaleWitnesses<PS>(initialPrivateState: PS) {
  return {
    divRemPow10Witness(
      context: { privateState?: PS },
      x: bigint,
      k: bigint,
    ): [PS, { q: bigint; r: bigint }] {
      const divisor = 10n ** k;
      const q = x / divisor;
      const r = x - q * divisor;
      // Pass the runtime's current private state through unchanged rather
      // than clobbering the store with the creation-time value.
      return [context.privateState ?? initialPrivateState, { q, r }];
    },
  };
}
