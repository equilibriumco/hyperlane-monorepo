/**
 * Per-contract owner state, ported from hyperlane-midnight. The ZOwnablePK
 * substrate binds the owner id to `secretNonce` (private state) +
 * `instanceSalt` (constructor immutable). Both are generated once per
 * contract and persisted so admin tooling can use the same identity later.
 * The maintenance-authority signing key sampled at deploy time is persisted
 * too — losing it means never being able to add or replace verifier keys
 * on that contract instance again.
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
  // Keyed by contract address; stored as whatever JSON value the SDK's
  // SigningKey serializes to.
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

  private save(state: OwnerStateFile): void {
    fs.mkdirSync(this.stateDir, { recursive: true });
    fs.writeFileSync(this.file, JSON.stringify(state, null, 2) + '\n');
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

  saveMaintenanceSigningKey(
    contractAddress: string,
    signingKey: unknown,
  ): void {
    const all = this.load();
    all.maintenanceSigningKeys[contractAddress] = signingKey;
    this.save(all);
  }

  getMaintenanceSigningKey(contractAddress: string): unknown {
    return this.load().maintenanceSigningKeys[contractAddress] ?? null;
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
