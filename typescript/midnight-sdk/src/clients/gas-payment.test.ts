import { expect } from 'chai';

import {
  findLandedGasPayment,
  payForGasWithLandingCheck,
  type LandedGasPayment,
  type PayForGasAttemptDeps,
} from './gas-payment.js';
import { readGasPayments, type GasPaymentRow } from './state.js';

const MESSAGE_ID = `0x${'ab'.repeat(32)}`;
const OTHER_ID = `0x${'cd'.repeat(32)}`;

function row(overrides: Partial<GasPaymentRow> = {}): GasPaymentRow {
  return {
    index: 0,
    messageId: MESSAGE_ID,
    destination: 11155111,
    gasAmount: '150000',
    payment: '150000',
    ...overrides,
  };
}

describe('findLandedGasPayment', () => {
  it('matches a row for the message that covers the intended payment', () => {
    const landed = findLandedGasPayment(
      [row({ index: 4 })],
      MESSAGE_ID,
      150000n,
    );
    expect(landed?.index).to.equal(4);
  });

  it('ignores rows for other messages', () => {
    const rows = [row({ messageId: OTHER_ID, payment: '999999' })];
    expect(findLandedGasPayment(rows, MESSAGE_ID, 150000n)).to.be.undefined;
  });

  // A third party can fund the same message for less; treating that as this
  // call's payment would leave the intended amount short.
  it('ignores a row that does not cover the intended payment', () => {
    const rows = [row({ payment: '149999' })];
    expect(findLandedGasPayment(rows, MESSAGE_ID, 150000n)).to.be.undefined;
  });

  it('compares message ids without regard to prefix or case', () => {
    const rows = [row({ messageId: MESSAGE_ID.slice(2).toUpperCase() })];
    expect(findLandedGasPayment(rows, MESSAGE_ID, 150000n)?.index).to.equal(0);
  });
});

describe('payForGasWithLandingCheck', () => {
  function harness(
    pay: () => Promise<{ txId: string }>,
    findLanded: () => Promise<LandedGasPayment | undefined>,
  ) {
    const calls = { pay: 0, findLanded: 0, sleep: 0 };
    const deps: PayForGasAttemptDeps = {
      pay: () => {
        calls.pay++;
        return pay();
      },
      findLanded: () => {
        calls.findLanded++;
        return findLanded();
      },
      attempts: 3,
      delayMs: 1,
      sleep: async () => {
        calls.sleep++;
      },
    };
    return { calls, deps };
  }

  const never = async (): Promise<LandedGasPayment | undefined> => undefined;

  it('pays once and never reads state when the attempt succeeds', async () => {
    const { calls, deps } = harness(async () => ({ txId: '0xtx' }), never);

    const outcome = await payForGasWithLandingCheck(deps);

    expect(outcome).to.eql({ kind: 'paid', txId: '0xtx' });
    expect(calls.pay).to.equal(1);
    expect(calls.findLanded).to.equal(0);
  });

  // The regression: a payForGas that was broadcast can still fail its
  // confirmation wait, and the contract keeps every payment, so a blind
  // retry charges twice.
  it('does not pay again when the failed attempt landed', async () => {
    const { calls, deps } = harness(
      async () => {
        throw new Error('timed out waiting for tx data');
      },
      async () => ({ index: 7, payment: '150000' }),
    );

    const outcome = await payForGasWithLandingCheck(deps);

    expect(outcome).to.eql({ kind: 'recovered', index: 7 });
    expect(calls.pay).to.equal(1);
    expect(calls.sleep).to.equal(0);
  });

  it('retries when a fresh read does not see the payment', async () => {
    let attempt = 0;
    const { calls, deps } = harness(async () => {
      attempt++;
      if (attempt === 1) throw new Error('proving failed');
      return { txId: '0xsecond' };
    }, never);

    const outcome = await payForGasWithLandingCheck(deps);

    expect(outcome).to.eql({ kind: 'paid', txId: '0xsecond' });
    expect(calls.pay).to.equal(2);
    expect(calls.sleep).to.equal(1);
  });

  // The failure that hides a landed payment is usually indexer trouble,
  // which also breaks the check — so an unreadable ledger must stop the
  // loop, not license another payment.
  it('stops without retrying when state cannot be read', async () => {
    const { calls, deps } = harness(
      async () => {
        throw new Error('timed out waiting for tx data');
      },
      async () => {
        throw new Error('indexer unreachable');
      },
    );

    const outcome = await payForGasWithLandingCheck(deps);

    expect(outcome.kind).to.equal('failed');
    if (outcome.kind !== 'failed') throw new Error('unreachable');
    expect(outcome.absence).to.equal('unknown');
    expect(calls.pay).to.equal(1);
  });

  it('reports the payment unseen after exhausting attempts', async () => {
    const { calls, deps } = harness(async () => {
      throw new Error('proving failed');
    }, never);

    const outcome = await payForGasWithLandingCheck(deps);

    expect(outcome.kind).to.equal('failed');
    if (outcome.kind !== 'failed') throw new Error('unreachable');
    expect(outcome.absence).to.equal('not-seen');
    expect(calls.pay).to.equal(3);
    expect(calls.findLanded).to.equal(3);
  });
});

// Pins the gas_payments slot and its atom order: the decoder is what the
// landing check reads, so drift there would silently reintroduce the
// double payment.
describe('readGasPayments', () => {
  type Atoms = Uint8Array[];

  function le(value: bigint, width: number): Uint8Array {
    const out = new Uint8Array(width);
    let rest = value;
    for (let i = 0; i < width; i++) {
      out[i] = Number(rest & 0xffn);
      rest >>= 8n;
    }
    return out;
  }

  function fakeMap(entries: { key: Atoms; value: Atoms }[]) {
    return {
      type: () => 'map',
      asMap: () => ({
        keys: () => entries.map((entry) => ({ value: entry.key })),
        get: (key: { value: Atoms }) => {
          const found = entries.find((entry) => entry.key === key.value);
          return found ? { asCell: () => ({ value: found.value }) } : undefined;
        },
      }),
    };
  }

  function cellSlots(): unknown[] {
    return Array.from({ length: 6 }, () => ({ type: () => 'cell' }));
  }

  function stateWith(map: unknown): unknown {
    const slots = cellSlots();
    slots[5] = map;
    return { state: { asArray: () => slots } };
  }

  it('decodes rows, including trimmed zero atoms at index 0', () => {
    const messageId = new Uint8Array(32).fill(0xab);
    const state = stateWith(
      fakeMap([
        {
          // index 0 and destination 0 trim to empty atoms on chain.
          key: [new Uint8Array(0)],
          value: [messageId, new Uint8Array(0), le(150000n, 8), le(7n, 16)],
        },
        {
          key: [le(1n, 4)],
          value: [
            new Uint8Array(32).fill(0xcd),
            le(11155111n, 4),
            le(200000n, 8),
            le(300000n, 16),
          ],
        },
      ]),
    );

    expect(readGasPayments(state)).to.eql([
      {
        index: 0,
        messageId: `0x${'ab'.repeat(32)}`,
        destination: 0,
        gasAmount: '150000',
        payment: '7',
      },
      {
        index: 1,
        messageId: `0x${'cd'.repeat(32)}`,
        destination: 11155111,
        gasAmount: '200000',
        payment: '300000',
      },
    ]);
  });

  // The defect this whole check exists to prevent. A Bytes<32> leaf is stored
  // with its trailing zero bytes dropped (verified against the ledger type: one
  // zero byte stores as 31, two as 30), so a messageId ending in 0x00 arrives
  // short. Hexing the raw atom gave a 62-character string that could never
  // equal the intended id, the landing check reported the payment unseen, and
  // the retry paid a second time — roughly one message in 256.
  it('right-pads a messageId whose trailing zero byte was trimmed', () => {
    const state = stateWith(
      fakeMap([
        {
          key: [new Uint8Array(0)],
          value: [
            new Uint8Array(31).fill(0xab),
            le(11155111n, 4),
            le(150000n, 8),
            le(150000n, 16),
          ],
        },
      ]),
    );

    const rows = readGasPayments(state);
    const intendedId = `0x${'ab'.repeat(31)}00`;

    expect(rows[0].messageId).to.equal(intendedId);
    expect(findLandedGasPayment(rows, intendedId, 150000n)?.index).to.equal(0);
  });

  it('rejects an over-long messageId atom', () => {
    const state = stateWith(
      fakeMap([
        {
          key: [new Uint8Array(0)],
          value: [new Uint8Array(33), le(1n, 4), le(1n, 8), le(1n, 16)],
        },
      ]),
    );

    expect(() => readGasPayments(state)).to.throw(/Bytes<32>/);
  });

  it('rejects a slot that is not the payments map', () => {
    expect(() =>
      readGasPayments({ state: { asArray: () => cellSlots() } }),
    ).to.throw(/layout drift/);
  });
});
