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

function slotAt(data: unknown, path: number[], expected: string): StateValue {
  let slot = unwrapChargedState(data);
  for (const index of path) {
    slot = slot.asArray()[index]!;
  }
  if (slot.type() !== expected) {
    throw new Error(
      `contract state slot [${path.join(',')}] is ${slot.type()}, expected ${expected} (layout drift?)`,
    );
  }
  return slot;
}

export function readRemoteRouters(
  data: unknown,
): { domainId: number; router: string }[] {
  const map = slotAt(data, NIGHT_REMOTE_ROUTERS_PATH, 'map').asMap();
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

// igp.compact remote_gas_data at flat slot [4]; each value is the
// (exchangeRate, gasPrice) pair as two 16-byte LE atoms. Guarded by
// scripts/check-ledger-layout.mjs in hyperlane-midnight.
const IGP_REMOTE_GAS_DATA_PATH = [4];

export function readRemoteGasData(
  data: unknown,
): { domainId: number; tokenExchangeRate: string; gasPrice: string }[] {
  const map = slotAt(data, IGP_REMOTE_GAS_DATA_PATH, 'map').asMap();
  const entries: {
    domainId: number;
    tokenExchangeRate: string;
    gasPrice: string;
  }[] = [];
  for (const key of map.keys()) {
    const cell = map.get(key)?.asCell();
    if (!cell) continue;
    entries.push({
      domainId: Number(leBytesToBigint(key.value[0])),
      tokenExchangeRate: leBytesToBigint(cell.value[0]).toString(),
      gasPrice: leBytesToBigint(cell.value[1]).toString(),
    });
  }
  return entries;
}

// validator-announce.compact seals the night address at flat slot [5]
// (constructor argument, no reader circuit exposes it).
const VA_MAILBOX_PATH = [5];

export function readVaMailboxAddress(data: unknown): string {
  const cell = slotAt(data, VA_MAILBOX_PATH, 'cell').asCell();
  return bytesToHex(cell.value[0]);
}

export function topLevelArity(data: unknown): number {
  return unwrapChargedState(data).asArray().length;
}
