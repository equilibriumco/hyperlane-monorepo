import { bytesToHex } from '../utils/conversion.js';

type AlignedValue = { value: Uint8Array[] };

type StateValue = {
  type(): string;
  asArray(): StateValue[];
  asCell(): AlignedValue;
  asMap(): {
    keys(): Iterable<AlignedValue>;
    get(key: AlignedValue): StateValue | undefined;
  };
};

// Positional path pinned to the compiled layout, like the Rust decoder's
// paths — guarded by scripts/check-ledger-layout.mjs in hyperlane-midnight.
const NIGHT_REMOTE_ROUTERS_PATH = [1, 0];

export function unwrapChargedState(data: unknown): StateValue {
  return (data as { state: StateValue }).state;
}

function leBytesToBigint(bytes: Uint8Array): bigint {
  let value = 0n;
  for (let i = bytes.length - 1; i >= 0; i--) {
    value = (value << 8n) | BigInt(bytes[i]);
  }
  return value;
}

export function readRemoteRouters(
  data: unknown,
): { domainId: number; router: string }[] {
  let slot = unwrapChargedState(data);
  for (const index of NIGHT_REMOTE_ROUTERS_PATH) {
    slot = slot.asArray()[index]!;
  }
  if (slot.type() !== 'map') {
    throw new Error(
      `night remote_routers slot [${NIGHT_REMOTE_ROUTERS_PATH.join(',')}] is ${slot.type()}, expected map (layout drift?)`,
    );
  }
  const map = slot.asMap();
  const routers: { domainId: number; router: string }[] = [];
  for (const key of map.keys()) {
    const cell = map.get(key)?.asCell();
    if (!cell) continue;
    routers.push({
      domainId: Number(leBytesToBigint(key.value[0])),
      router: bytesToHex(cell.value[0]),
    });
  }
  return routers;
}
