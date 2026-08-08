// Permanent off-chain implementation of power-of-10 division and
// multiplication (Compact has no integer division and no Uint wider than
// 128 bits); the circuits bind each result in Field arithmetic. Mirrors
// contracts/src/witnesses/scale-witnesses.ts in the hyperlane-midnight
// repo — keep in sync.
const UINT128_MASK = (1n << 128n) - 1n;

export function createScaleWitnesses<PS>(privateState: PS) {
  return {
    divRemPow10Witness(
      _context: unknown,
      x: bigint,
      k: bigint,
    ): [PS, { q: bigint; r: bigint }] {
      const divisor = 10n ** k;
      const q = x / divisor;
      const r = x - q * divisor;
      return [privateState, { q, r }];
    },

    mulPow10Witness(_context: unknown, x: bigint, k: bigint): [PS, bigint] {
      const product = x * 10n ** k;
      // Narrow to Uint<128>; an actual overflow produces a value that
      // disagrees with the Field-arithmetic equality the circuit checks.
      return [privateState, product & UINT128_MASK];
    },
  };
}
