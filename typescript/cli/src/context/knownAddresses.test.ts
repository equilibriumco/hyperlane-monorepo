import { expect } from 'chai';

import type { ChainAddresses, IRegistry } from '@hyperlane-xyz/registry';
import type {
  ChainMap,
  ChainMetadata,
  WarpCoreConfig,
  WarpRouteDeployConfig,
} from '@hyperlane-xyz/sdk';
import { ProtocolType } from '@hyperlane-xyz/utils';

import {
  type KnownAddresses,
  KnownAddressesSchema,
  type LoadedAddress,
  compareKnownAddresses,
  loadRegistryAddresses,
} from './knownAddresses.js';

const NIGHT =
  '1111111111111111111111111111111111111111111111111111111111111111';
const IGP = '2222222222222222222222222222222222222222222222222222222222222222';
const VA = '3333333333333333333333333333333333333333333333333333333333333333';
const OLD_NIGHT =
  'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa';

const known: KnownAddresses = KnownAddressesSchema.parse({
  route: 'NIGHT/midnight1-test4',
  midnight: { chainName: 'midnight1', network: 'devnet', domain: 1234 },
  remote: { chainName: 'test4', domain: 31338 },
  deployments: [
    {
      name: 'July 2026',
      deployed: '2026-07-24',
      midnight: { night: OLD_NIGHT, igp: IGP, validatorAnnounce: VA },
      remote: {
        tokenRouter: '0x00000000000000000000000000000000000000a1',
        tokenIsm: '0x00000000000000000000000000000000000000e1',
        mailbox: '0x00000000000000000000000000000000000000b1',
        merkleTreeHook: '0x00000000000000000000000000000000000000c1',
        interchainGasPaymaster: '0x00000000000000000000000000000000000000d1',
      },
    },
    {
      name: 'August 2026',
      deployed: '2026-08-20',
      midnight: { night: NIGHT, igp: IGP, validatorAnnounce: VA },
      remote: {
        tokenRouter: '0x00000000000000000000000000000000000000a2',
        tokenIsm: '0x00000000000000000000000000000000000000e2',
        mailbox: '0x00000000000000000000000000000000000000b2',
        merkleTreeHook: '0x00000000000000000000000000000000000000c2',
        interchainGasPaymaster: '0x00000000000000000000000000000000000000d2',
      },
    },
  ],
});

// The registry as it serves the August deployment: Midnight addresses bare,
// EVM addresses checksummed.
const midnightAddresses: ChainAddresses = {
  mailbox: NIGHT,
  merkleTreeHook: NIGHT,
  interchainGasPaymaster: IGP,
  validatorAnnounce: VA,
};
const remoteAddresses: ChainAddresses = {
  mailbox: '0x00000000000000000000000000000000000000B2',
  merkleTreeHook: '0x00000000000000000000000000000000000000C2',
  interchainGasPaymaster: '0x00000000000000000000000000000000000000D2',
  validatorAnnounce: '0x00000000000000000000000000000000000000F2',
};
const route = {
  tokens: [
    { chainName: 'midnight1', addressOrDenom: NIGHT },
    {
      chainName: 'test4',
      addressOrDenom: '0x00000000000000000000000000000000000000A2',
    },
  ],
} as WarpCoreConfig;
const deployConfig = {
  test4: {
    interchainSecurityModule: '0x00000000000000000000000000000000000000E2',
  },
} as unknown as WarpRouteDeployConfig;

const chainMetadata = {
  midnight1: { domainId: 1234, protocol: ProtocolType.Midnight },
  test4: { domainId: 31338, protocol: ProtocolType.Ethereum },
} as unknown as ChainMap<ChainMetadata>;

interface FakeRegistry {
  addresses: ChainMap<ChainAddresses>;
  warpRoute?: WarpCoreConfig | null;
  warpDeploy?: WarpRouteDeployConfig | null;
}

function registry({
  addresses,
  warpRoute = route,
  warpDeploy = deployConfig,
}: FakeRegistry): IRegistry {
  return {
    getChainAddresses: async (chain: string) => addresses[chain] ?? null,
    getWarpRoute: async () => warpRoute,
    getWarpDeployConfig: async () => warpDeploy,
  } as unknown as IRegistry;
}

const augustRegistry = { midnight1: midnightAddresses, test4: remoteAddresses };

async function load(fake: FakeRegistry): Promise<LoadedAddress[]> {
  const loaded = await loadRegistryAddresses(
    registry(fake),
    chainMetadata,
    known,
  );
  expect(loaded).to.not.equal(null);
  return loaded!;
}

describe('loadRegistryAddresses', () => {
  it('collects the core addresses, the route tokens and the remote ISM', async () => {
    const loaded = await load({ addresses: augustRegistry });

    expect(loaded).to.eql([
      { key: 'midnight.night', value: NIGHT },
      { key: 'midnight.night', value: NIGHT },
      { key: 'midnight.igp', value: IGP },
      { key: 'midnight.validatorAnnounce', value: VA },
      { key: 'remote.mailbox', value: remoteAddresses.mailbox },
      { key: 'remote.merkleTreeHook', value: remoteAddresses.merkleTreeHook },
      {
        key: 'remote.interchainGasPaymaster',
        value: remoteAddresses.interchainGasPaymaster,
      },
      { key: 'midnight.night', value: NIGHT },
      {
        key: 'remote.tokenRouter',
        value: '0x00000000000000000000000000000000000000A2',
      },
      {
        key: 'remote.tokenIsm',
        value: '0x00000000000000000000000000000000000000E2',
      },
    ]);
  });

  it('skips a named chain the registry does not have', async () => {
    const loaded = await load({ addresses: { midnight1: midnightAddresses } });
    expect(loaded.map((e) => e.key)).to.eql([
      'midnight.night',
      'midnight.night',
      'midnight.igp',
      'midnight.validatorAnnounce',
      'midnight.night',
    ]);
  });

  it('returns null when the registry has neither chain', async () => {
    const loaded = await loadRegistryAddresses(
      registry({ addresses: { anvil1: remoteAddresses } }),
      chainMetadata,
      known,
    );
    expect(loaded).to.equal(null);
  });

  it('fails when a chain domain differs from the known list', async () => {
    let error: Error | undefined;
    try {
      await loadRegistryAddresses(
        registry({ addresses: augustRegistry }),
        {
          ...chainMetadata,
          midnight1: { ...chainMetadata.midnight1, domainId: 4321 },
        },
        known,
      );
    } catch (e) {
      error = e as Error;
    }
    expect(error?.message).to.equal(
      'registry address check failed: midnight.domain is 4321, the known list has 1234',
    );
  });
});

describe('compareKnownAddresses', () => {
  it('passes when the registry matches one deployment entry', async () => {
    const loaded = await load({ addresses: augustRegistry });
    expect(compareKnownAddresses(loaded, known)).to.equal(undefined);
  });

  it('compares hex case-insensitively and with or without 0x', async () => {
    const loaded = await load({
      addresses: {
        midnight1: {
          ...midnightAddresses,
          mailbox: `0x${NIGHT.toUpperCase()}`,
        },
        test4: {
          ...remoteAddresses,
          mailbox: '00000000000000000000000000000000000000b2',
        },
      },
      warpRoute: {
        tokens: [
          { chainName: 'midnight1', addressOrDenom: `0x${NIGHT}` },
          {
            chainName: 'test4',
            addressOrDenom: '0x00000000000000000000000000000000000000a2',
          },
        ],
      } as WarpCoreConfig,
    });
    expect(compareKnownAddresses(loaded, known)).to.equal(undefined);
  });

  it('names the first tampered address of the closest entry', async () => {
    const tampered = `0x${'ff'.repeat(20)}`;
    const loaded = await load({
      addresses: {
        midnight1: midnightAddresses,
        test4: { ...remoteAddresses, merkleTreeHook: tampered },
      },
    });
    expect(compareKnownAddresses(loaded, known)).to.equal(
      `registry address check failed: remote.merkleTreeHook is ${tampered}, the known list has 0x00000000000000000000000000000000000000c2 (deployment "August 2026")`,
    );
  });

  it('catches a tampered route token', async () => {
    const loaded = await load({
      addresses: augustRegistry,
      warpRoute: {
        tokens: [
          { chainName: 'midnight1', addressOrDenom: OLD_NIGHT },
          {
            chainName: 'test4',
            addressOrDenom: '0x00000000000000000000000000000000000000a2',
          },
        ],
      } as WarpCoreConfig,
    });
    expect(compareKnownAddresses(loaded, known)).to.equal(
      `registry address check failed: midnight.night is ${OLD_NIGHT}, the known list has ${NIGHT} (deployment "August 2026")`,
    );
  });

  it('catches a tampered remote token ISM', async () => {
    const loaded = await load({
      addresses: augustRegistry,
      warpDeploy: {
        test4: {
          interchainSecurityModule:
            '0x00000000000000000000000000000000000000e9',
        },
      } as unknown as WarpRouteDeployConfig,
    });
    expect(compareKnownAddresses(loaded, known)).to.equal(
      'registry address check failed: remote.tokenIsm is 0x00000000000000000000000000000000000000e9, the known list has 0x00000000000000000000000000000000000000e2 (deployment "August 2026")',
    );
  });

  it('reports against the entry with the most matching fields', async () => {
    // The midnight fields match July, the five remote fields match August.
    const loaded = await load({
      addresses: {
        midnight1: {
          ...midnightAddresses,
          mailbox: OLD_NIGHT,
          merkleTreeHook: OLD_NIGHT,
        },
        test4: remoteAddresses,
      },
      warpRoute: null,
    });
    expect(compareKnownAddresses(loaded, known)).to.equal(
      `registry address check failed: midnight.night is ${OLD_NIGHT}, the known list has ${NIGHT} (deployment "August 2026")`,
    );
  });

  it('skips null fields, as recorded before the remote side exists', async () => {
    const partial = KnownAddressesSchema.parse({
      ...known,
      deployments: [
        {
          name: 'September 2026',
          deployed: '2026-09-01',
          midnight: { night: NIGHT, igp: IGP, validatorAnnounce: VA },
          remote: {
            tokenRouter: null,
            tokenIsm: null,
            mailbox: null,
            merkleTreeHook: null,
            interchainGasPaymaster: null,
          },
        },
      ],
    });
    const loaded = await load({ addresses: augustRegistry });
    expect(compareKnownAddresses(loaded, partial)).to.equal(undefined);
  });

  it('ignores known fields the registry does not carry', () => {
    const loaded = [{ key: 'midnight.night', value: NIGHT }];
    expect(compareKnownAddresses(loaded, known)).to.equal(undefined);
  });
});
