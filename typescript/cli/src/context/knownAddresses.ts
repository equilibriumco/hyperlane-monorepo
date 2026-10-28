import { z } from 'zod';

import type { IRegistry } from '@hyperlane-xyz/registry';
import type { ChainMap, ChainMetadata } from '@hyperlane-xyz/sdk';
import { ProtocolType, assert } from '@hyperlane-xyz/utils';

import { warnYellow } from '../logger.js';
import { readJson } from '../utils/files.js';

import type { CommandContext } from './types.js';

export const KNOWN_ADDRESSES_ENV = 'HYPERLANE_KNOWN_ADDRESSES';

export const KNOWN_ADDRESSES_DEPLOY_NOTE =
  'Add the deployed addresses to the known list (deployments/stagenet-sepolia.json in hyperlane-midnight-private) from this deploy record, not from the registry.';

// `core deploy`, `warp deploy` and the other deploys create new addresses, so
// they are the only commands that skip the check.
const DEPLOY_COMMAND = 'deploy';

const FAILED = 'registry address check failed';

// Registry core address fields that carry a known-list address, in report
// order. The night contract is the Midnight mailbox and its merkle tree hook.
const MIDNIGHT_FIELDS: [string, string][] = [
  ['midnight.night', 'mailbox'],
  ['midnight.night', 'merkleTreeHook'],
  ['midnight.igp', 'interchainGasPaymaster'],
  ['midnight.validatorAnnounce', 'validatorAnnounce'],
];
const REMOTE_FIELDS: [string, string][] = [
  ['remote.mailbox', 'mailbox'],
  ['remote.merkleTreeHook', 'merkleTreeHook'],
  ['remote.interchainGasPaymaster', 'interchainGasPaymaster'],
];

// A null address is one the list does not know yet and is skipped.
const KnownAddress = z
  .string()
  .regex(/^(0x)?[0-9a-fA-F]+$/)
  .nullable();

const DeploymentSchema = z.object({
  name: z.string(),
  deployed: z.string(),
  midnight: z.object({
    night: KnownAddress,
    igp: KnownAddress,
    validatorAnnounce: KnownAddress,
  }),
  remote: z.record(KnownAddress),
});

export const KnownAddressesSchema = z.object({
  route: z.string(),
  midnight: z.object({
    chainName: z.string(),
    network: z.string(),
    domain: z.number().int(),
  }),
  remote: z.object({
    chainName: z.string(),
    domain: z.number().int(),
  }),
  deployments: z.array(DeploymentSchema).nonempty(),
});

export type KnownAddresses = z.infer<typeof KnownAddressesSchema>;
type KnownDeployment = KnownAddresses['deployments'][number];

// One address as the registry served it, keyed the way the known list names
// it (`midnight.night`, `remote.mailbox`, ...).
export interface LoadedAddress {
  key: string;
  value: string;
}

function normalizeHex(value: string): string {
  return value.toLowerCase().replace(/^0x/, '').padStart(64, '0');
}

function knownValue(
  deployment: KnownDeployment,
  key: string,
): string | undefined {
  const [side, field] = key.split('.');
  const addresses: Record<string, string | null> =
    side === 'midnight' ? deployment.midnight : deployment.remote;
  return addresses[field] ?? undefined;
}

/**
 * A deployment entry matches when every address it knows that was also
 * loaded is equal. Returns the failure message naming the first changed
 * address of the entry with the most equal addresses, or undefined when an
 * entry matches.
 */
export function compareKnownAddresses(
  loaded: LoadedAddress[],
  known: KnownAddresses,
): string | undefined {
  let closest:
    | {
        deployment: KnownDeployment;
        matched: number;
        mismatch: LoadedAddress & { known: string };
      }
    | undefined;

  for (const deployment of known.deployments) {
    let matched = 0;
    let mismatch: (LoadedAddress & { known: string }) | undefined;
    for (const entry of loaded) {
      const expected = knownValue(deployment, entry.key);
      if (expected === undefined) continue;
      if (normalizeHex(expected) === normalizeHex(entry.value)) {
        matched += 1;
      } else {
        mismatch ??= { ...entry, known: expected };
      }
    }
    if (!mismatch) return undefined;
    if (!closest || matched > closest.matched) {
      closest = { deployment, matched, mismatch };
    }
  }

  assert(closest, 'the known list has no deployments');
  const { deployment, mismatch } = closest;
  return `${FAILED}: ${mismatch.key} is ${mismatch.value}, the known list has ${mismatch.known} (deployment "${deployment.name}")`;
}

/**
 * Collects the addresses the registry serves for the chains the known list
 * names: their core addresses, the route's token on each and the remote
 * token's ISM. A chain the registry lacks is skipped; null when it lacks
 * both, meaning there is nothing to check.
 */
export async function loadRegistryAddresses(
  registry: IRegistry,
  chainMetadata: ChainMap<ChainMetadata>,
  known: KnownAddresses,
): Promise<LoadedAddress[] | null> {
  const loaded: LoadedAddress[] = [];
  const present = new Set<string>();
  const sides = [
    ['midnight', known.midnight, MIDNIGHT_FIELDS],
    ['remote', known.remote, REMOTE_FIELDS],
  ] as const;

  for (const [side, chain, fields] of sides) {
    const addresses = await registry.getChainAddresses(chain.chainName);
    if (!addresses) continue;
    present.add(chain.chainName);
    const domainId = chainMetadata[chain.chainName]?.domainId;
    if (domainId !== undefined && domainId !== chain.domain) {
      throw new Error(
        `${FAILED}: ${side}.domain is ${domainId}, the known list has ${chain.domain}`,
      );
    }
    for (const [key, registryField] of fields) {
      const value = addresses[registryField];
      if (value) loaded.push({ key, value });
    }
  }
  if (present.size === 0) return null;

  const route = await registry.getWarpRoute(known.route);
  for (const token of route?.tokens ?? []) {
    if (!token.addressOrDenom || !present.has(token.chainName)) continue;
    if (token.chainName === known.midnight.chainName) {
      loaded.push({ key: 'midnight.night', value: token.addressOrDenom });
    } else if (token.chainName === known.remote.chainName) {
      loaded.push({ key: 'remote.tokenRouter', value: token.addressOrDenom });
    }
  }

  if (present.has(known.remote.chainName)) {
    const deployConfig = await registry.getWarpDeployConfig(known.route);
    const ism =
      deployConfig?.[known.remote.chainName]?.interchainSecurityModule;
    if (typeof ism === 'string') {
      loaded.push({ key: 'remote.tokenIsm', value: ism });
    }
  }

  return loaded;
}

export async function checkRegistryAddresses(
  context: Pick<CommandContext, 'registry' | 'chainMetadata'>,
  knownListPath: string,
): Promise<void> {
  const known = KnownAddressesSchema.parse(readJson(knownListPath));
  const loaded = await loadRegistryAddresses(
    context.registry,
    context.chainMetadata,
    known,
  );
  if (!loaded) return;
  const failure = compareKnownAddresses(loaded, known);
  if (failure) throw new Error(failure);
}

export async function knownAddressesMiddleware(
  argv: Record<string, unknown> & { context?: CommandContext },
): Promise<void> {
  const positional = Array.isArray(argv._) ? argv._ : [];
  if (positional.includes(DEPLOY_COMMAND) || !argv.context) return;
  // Only a registry that serves a Midnight chain is checked, so commands on
  // other chains behave as upstream's.
  const servesMidnight = Object.values(argv.context.chainMetadata).some(
    (metadata) => metadata.protocol === ProtocolType.Midnight,
  );
  if (!servesMidnight) return;

  const knownListPath = process.env[KNOWN_ADDRESSES_ENV];
  if (!knownListPath) {
    warnYellow(
      `Registry addresses were not checked against a known list (${KNOWN_ADDRESSES_ENV} is unset)`,
    );
    return;
  }
  await checkRegistryAddresses(argv.context, knownListPath);
}
