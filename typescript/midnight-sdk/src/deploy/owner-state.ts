/**
 * Per-contract owner state. ZOwnablePK binds the owner id to a `secretNonce` in
 * private state plus an immutable `instanceSalt`, both generated once per
 * contract and persisted so admin tooling keeps the same identity later. The
 * maintenance committee keys this host holds are persisted too: the committee
 * needs two signatures, so once fewer than two of its keys survive, no
 * verifier key can ever be added or replaced on that instance again.
 */
import * as crypto from 'node:crypto';
import * as fs from 'node:fs';
import * as path from 'node:path';

import type { MidnightContractName } from '../clients/contracts.js';
import { bytesToHex, hexToBytes } from '../utils/conversion.js';

interface OwnerStateEntry {
  secretNonce: string;
  instanceSalt: string;
}

interface OwnerStateFile {
  contracts: Partial<Record<MidnightContractName, OwnerStateEntry>>;
  // Keyed by contract address: the committee keys this host holds, each at
  // its committee index, as the JSON the SDK's SigningKey serializes to. A
  // deploy that handed maintenance to keys held elsewhere stores an empty
  // list; an entry written before committees holds one key, not a list.
  maintenanceSigningKeys: Record<string, unknown>;
}

export interface OwnerState {
  secretNonce: Uint8Array;
  instanceSalt: Uint8Array;
}

export class OwnerStateStore {
  private readonly file: string;

  constructor(readonly stateDir: string) {
    this.file = path.join(stateDir, 'owner-state.json');
  }

  privateStateStorePath(): string {
    return path.join(this.stateDir, 'private-state');
  }

  private load(): OwnerStateFile {
    if (!fs.existsSync(this.file)) {
      return { contracts: {}, maintenanceSigningKeys: {} };
    }
    const parsed = JSON.parse(
      fs.readFileSync(this.file, 'utf-8'),
    ) as Partial<OwnerStateFile>;
    return {
      contracts: parsed.contracts ?? {},
      maintenanceSigningKeys: parsed.maintenanceSigningKeys ?? {},
    };
  }

  // The file holds every contract's owner secret and maintenance key, so only
  // its owner may read it. The modes apply when the directory or file is
  // created; the chmod tightens a file an older run left readable.
  private save(state: OwnerStateFile): void {
    fs.mkdirSync(this.stateDir, { recursive: true, mode: 0o700 });
    fs.writeFileSync(this.file, JSON.stringify(state, null, 2) + '\n', {
      mode: 0o600,
    });
    fs.chmodSync(this.file, 0o600);
  }

  // A deploy adds its secrets to the file, so it stops while the file is
  // readable by others: whoever could read it may already hold the secrets.
  assertPrivate(): void {
    if (!fs.existsSync(this.file)) return;
    if ((fs.statSync(this.file).mode & 0o077) !== 0) {
      throw new Error(
        `${this.file} is readable by other users. It holds each contract's ` +
          'owner secret and maintenance key: check who could have read it, ' +
          'then chmod 600 it and deploy again.',
      );
    }
  }

  getOrCreate(contract: MidnightContractName): OwnerState {
    const all = this.load();
    const existing = all.contracts[contract];
    if (existing) {
      return {
        secretNonce: hexToBytes(existing.secretNonce),
        instanceSalt: hexToBytes(existing.instanceSalt),
      };
    }
    const fresh: OwnerState = {
      secretNonce: new Uint8Array(crypto.randomBytes(32)),
      instanceSalt: new Uint8Array(crypto.randomBytes(32)),
    };
    all.contracts[contract] = {
      secretNonce: bytesToHex(fresh.secretNonce),
      instanceSalt: bytesToHex(fresh.instanceSalt),
    };
    this.save(all);
    return fresh;
  }

  get(contract: MidnightContractName): OwnerState | null {
    const entry = this.load().contracts[contract];
    if (!entry) return null;
    return {
      secretNonce: hexToBytes(entry.secretNonce),
      instanceSalt: hexToBytes(entry.instanceSalt),
    };
  }

  saveMaintenanceSigningKeys(
    contractAddress: string,
    signingKeys: unknown[],
  ): void {
    const all = this.load();
    all.maintenanceSigningKeys[contractAddress] = signingKeys;
    this.save(all);
  }

  // The committee keys this host holds, in committee order.
  getMaintenanceSigningKeys(contractAddress: string): unknown[] {
    const stored = this.load().maintenanceSigningKeys[contractAddress];
    if (stored == null) return [];
    return Array.isArray(stored) ? stored : [stored];
  }
}

export type ComputeOwnerId = (pk: PkEither, nonce: Uint8Array) => Uint8Array;

export interface PkEither {
  is_left: boolean;
  left: { bytes: Uint8Array };
  right: { bytes: Uint8Array };
}

export function userPkEither(coinPkBytes: Uint8Array): PkEither {
  return {
    is_left: true,
    left: { bytes: coinPkBytes },
    right: { bytes: new Uint8Array(32) },
  };
}

export function decodeCoinPublicKey(hex: string): Uint8Array {
  const bytes = hexToBytes(hex);
  if (bytes.length !== 32) {
    throw new Error(
      `coinPublicKey hex must decode to 32 bytes, got ${bytes.length}: ${hex}`,
    );
  }
  return bytes;
}
