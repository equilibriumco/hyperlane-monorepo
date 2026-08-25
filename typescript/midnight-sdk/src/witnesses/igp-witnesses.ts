// The gas-payment division stays off-chain: its three-factor numerator exceeds
// Uint<128>, so it cannot go through Scale.divRemPow10, and the circuit binds
// (q, r) to the true product in Field arithmetic instead. Mirrors the contracts
// repo's own witnesses — keep in sync.
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
