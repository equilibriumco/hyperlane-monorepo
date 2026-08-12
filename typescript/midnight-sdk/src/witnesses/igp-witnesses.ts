// Permanent off-chain implementation of the gas-payment division: the
// three-factor numerator exceeds Uint<128>, so it cannot go through
// Scale.divRemPow10. The circuit binds (q, r) to the true product in Field
// arithmetic. Mirrors contracts/src/witnesses/igp-witnesses.ts in the
// hyperlane-midnight repo — keep in sync.
const TOKEN_EXCHANGE_RATE_SCALE = 10n ** 10n;

export function createIgpWitnesses<PS>(initialPrivateState: PS) {
  return {
    quoteDivByScaleWitness(
      context: { privateState?: PS },
      gasLimit: bigint,
      gasPrice: bigint,
      exchangeRate: bigint,
    ): [PS, { q: bigint; r: bigint }] {
      const num = gasLimit * gasPrice * exchangeRate;
      const q = num / TOKEN_EXCHANGE_RATE_SCALE;
      const r = num - q * TOKEN_EXCHANGE_RATE_SCALE;
      // Pass the runtime's current private state through unchanged rather
      // than clobbering the store with the creation-time value.
      return [context.privateState ?? initialPrivateState, { q, r }];
    },
  };
}
