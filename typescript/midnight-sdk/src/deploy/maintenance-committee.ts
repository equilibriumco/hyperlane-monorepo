/**
 * The maintenance committee every Midnight deploy ends with. The deploy key is
 * a 1-of-1 authority only until the last transaction of the deploy, which
 * replaces it with a committee of at least three verifying keys and a
 * threshold of two, so no key on its own can change a verifier key or the
 * authority.
 */
import * as ledger from '@midnightntwrk/ledger-v9';
import type { SignatureVerifyingKey } from '@midnightntwrk/ledger-v9';

export const MAINTENANCE_THRESHOLD = 2;
export const MIN_COMMITTEE_SIZE = 3;

const KINDS: ReadonlyArray<SignatureVerifyingKey['tag']> = ['schnorr', 'ecdsa'];

/** The form `MIDNIGHT_MAINTENANCE_COMMITTEE` takes for one key: `<kind>:<hex>`. */
export function formatVerifyingKey(key: SignatureVerifyingKey): string {
  return `${key.tag}:${key.value}`;
}

export function parseCommittee(raw: string): SignatureVerifyingKey[] {
  const keys = raw
    .split(',')
    .map((s) => s.trim())
    .filter((s) => s.length > 0)
    .map((token) => {
      const [tag, value, ...rest] = token.split(':');
      if (
        !tag ||
        !value ||
        rest.length > 0 ||
        !KINDS.includes(tag as SignatureVerifyingKey['tag'])
      ) {
        throw new Error(
          `MIDNIGHT_MAINTENANCE_COMMITTEE: expected <kind>:<hex> with kind ${KINDS.join(' or ')}, got '${token}'`,
        );
      }
      if (!/^[0-9a-fA-F]+$/.test(value) || value.length % 2 !== 0) {
        throw new Error(
          `MIDNIGHT_MAINTENANCE_COMMITTEE: '${token}' is not a hex verifying key`,
        );
      }
      const key = {
        tag: tag as SignatureVerifyingKey['tag'],
        value: value.toLowerCase(),
      };
      // The ledger checks the key only at the hand-over, after the deploy.
      try {
        new ledger.ContractMaintenanceAuthority([key], 1, 0n);
      } catch (e) {
        throw new Error(
          `MIDNIGHT_MAINTENANCE_COMMITTEE: '${token}' is not a ${key.tag} verifying key: ${e instanceof Error ? e.message : String(e)}`,
        );
      }
      return key;
    });
  if (keys.length < MIN_COMMITTEE_SIZE) {
    throw new Error(
      `MIDNIGHT_MAINTENANCE_COMMITTEE: at least ${MIN_COMMITTEE_SIZE} keys are needed for a ` +
        `threshold of ${MAINTENANCE_THRESHOLD}, got ${keys.length}`,
    );
  }
  if (new Set(keys.map(formatVerifyingKey)).size !== keys.length) {
    throw new Error(
      'MIDNIGHT_MAINTENANCE_COMMITTEE: the same key is listed twice',
    );
  }
  return keys;
}

/** The committee the operator brings, or null for a sampled one. */
export function committeeFromEnv(): SignatureVerifyingKey[] | null {
  const raw = process.env.MIDNIGHT_MAINTENANCE_COMMITTEE;
  return raw ? parseCommittee(raw) : null;
}
