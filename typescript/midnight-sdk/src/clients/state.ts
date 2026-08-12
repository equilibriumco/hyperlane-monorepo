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

// Positional paths into the compiled contract layout. Adding a ledger field
// can move sibling slots, so a guard in the contracts repo re-checks them.
const NIGHT_REMOTE_ROUTERS_PATH = [0, 6];
const NIGHT_DESTINATION_GAS_PATH = [1, 14];

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

// A stored Bytes<N> leaf drops its trailing zero bytes, so a messageId ending
// in 0x00 reads back as 31 bytes. Pad it back; only over-long is an error.
function fixedBytesToHex(atom: Uint8Array | undefined, width: number): string {
  const bytes = atom ?? new Uint8Array(0);
  if (bytes.length > width) {
    throw new Error(
      `Bytes<${width}> leaf holds ${bytes.length} bytes (layout drift?)`,
    );
  }
  const padded = new Uint8Array(width);
  padded.set(bytes);
  return bytesToHex(padded);
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
      router: fixedBytesToHex(cell.value[0], 32),
    });
  }
  return routers;
}

// night.compact destination_gas: domain -> Uint<64> handle-gas defaults for
// warp quoting. A zero value stores as a trimmed (possibly absent) atom.
export function readDestinationGas(
  data: unknown,
): { domainId: number; gas: string }[] {
  const map = slotAt(data, NIGHT_DESTINATION_GAS_PATH, 'map').asMap();
  const entries: { domainId: number; gas: string }[] = [];
  for (const key of map.keys()) {
    const cell = map.get(key)?.asCell();
    if (!cell) continue;
    entries.push({
      domainId: Number(leBytesToBigint(key.value[0])),
      gas: leBytesToBigint(cell.value[0] ?? new Uint8Array(0)).toString(),
    });
  }
  return entries;
}

// igp.compact remote_gas_data at flat slot [4]. Each value holds exchangeRate
// and gasPrice as 16-byte LE atoms, then gasOverhead as an 8-byte one.
const IGP_REMOTE_GAS_DATA_PATH = [4];

export function readRemoteGasData(data: unknown): {
  domainId: number;
  tokenExchangeRate: string;
  gasPrice: string;
  gasOverhead: string;
}[] {
  const map = slotAt(data, IGP_REMOTE_GAS_DATA_PATH, 'map').asMap();
  const entries: {
    domainId: number;
    tokenExchangeRate: string;
    gasPrice: string;
    gasOverhead: string;
  }[] = [];
  for (const key of map.keys()) {
    const cell = map.get(key)?.asCell();
    if (!cell) continue;
    entries.push({
      domainId: Number(leBytesToBigint(key.value[0])),
      tokenExchangeRate: leBytesToBigint(cell.value[0]).toString(),
      gasPrice: leBytesToBigint(cell.value[1]).toString(),
      gasOverhead: leBytesToBigint(
        cell.value[2] ?? new Uint8Array(0),
      ).toString(),
    });
  }
  return entries;
}

// igp.compact gas_payments at flat slot [5]: an append-only log of
// (messageId, destination, gasAmount, payment) keyed by index.
const IGP_GAS_PAYMENTS_PATH = [5];

export type GasPaymentRow = {
  index: number;
  messageId: string;
  destination: number;
  gasAmount: string;
  payment: string;
};

export function readGasPayments(data: unknown): GasPaymentRow[] {
  const map = slotAt(data, IGP_GAS_PAYMENTS_PATH, 'map').asMap();
  const rows: GasPaymentRow[] = [];
  for (const key of map.keys()) {
    const cell = map.get(key)?.asCell();
    if (!cell) continue;
    rows.push({
      index: Number(leBytesToBigint(key.value[0] ?? new Uint8Array(0))),
      messageId: fixedBytesToHex(cell.value[0], 32),
      destination: Number(leBytesToBigint(cell.value[1] ?? new Uint8Array(0))),
      gasAmount: leBytesToBigint(cell.value[2] ?? new Uint8Array(0)).toString(),
      payment: leBytesToBigint(cell.value[3] ?? new Uint8Array(0)).toString(),
    });
  }
  return rows;
}

// validator-announce.compact seals the night address at flat slot [5]
// (constructor argument, no reader circuit exposes it).
const VA_MAILBOX_PATH = [5];

export function readVaMailboxAddress(data: unknown): string {
  const cell = slotAt(data, VA_MAILBOX_PATH, 'cell').asCell();
  return fixedBytesToHex(cell.value[0], 32);
}

export function topLevelArity(data: unknown): number {
  return unwrapChargedState(data).asArray().length;
}
