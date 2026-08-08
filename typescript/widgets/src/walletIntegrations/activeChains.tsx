import { useMemo } from 'react';

import type { MinimalProviderRegistry } from '@hyperlane-xyz/sdk/providers/MinimalProviderRegistry';
import { type KnownProtocolType, ProtocolType } from '@hyperlane-xyz/utils';

import { useAleoActiveChain } from './aleoWallet.js';
import { useMidnightActiveChain } from './midnight.js';
import { useCosmosActiveChain } from './cosmosWallet.js';
import { useEthereumActiveChain } from './ethereumWallet.js';
import { useRadixActiveChain } from './radixWallet.js';
import { useSolanaActiveChain } from './solanaWallet.js';
import { useStarknetActiveChain } from './starknetWallet.js';
import { type ActiveChainInfo } from './types.js';
import { useTronActiveChain } from './tronWallet.js';

export function useActiveChains(multiProvider: MinimalProviderRegistry): {
  chains: Record<KnownProtocolType, ActiveChainInfo>;
  readyChains: Array<ActiveChainInfo>;
} {
  const evmChain = useEthereumActiveChain(multiProvider);
  const solChain = useSolanaActiveChain(multiProvider);
  const cosmChain = useCosmosActiveChain(multiProvider);
  const starknetChain = useStarknetActiveChain(multiProvider);
  const radixChain = useRadixActiveChain(multiProvider);
  const aleoChain = useAleoActiveChain(multiProvider);
  const tronChain = useTronActiveChain(multiProvider);
  const midnightChain = useMidnightActiveChain(multiProvider);

  const readyChains = useMemo(
    () =>
      [
        evmChain,
        solChain,
        cosmChain,
        starknetChain,
        radixChain,
        aleoChain,
        tronChain,
        midnightChain,
      ].filter((c) => !!c.chainDisplayName),
    [
      evmChain,
      solChain,
      cosmChain,
      starknetChain,
      radixChain,
      aleoChain,
      tronChain,
      midnightChain,
    ],
  );

  return useMemo(
    () => ({
      chains: {
        [ProtocolType.Ethereum]: evmChain,
        [ProtocolType.Sealevel]: solChain,
        [ProtocolType.Cosmos]: cosmChain,
        [ProtocolType.CosmosNative]: cosmChain,
        [ProtocolType.Starknet]: starknetChain,
        [ProtocolType.Radix]: radixChain,
        [ProtocolType.Aleo]: aleoChain,
        [ProtocolType.Tron]: tronChain,
        [ProtocolType.Midnight]: midnightChain,
      },
      readyChains,
    }),
    [
      evmChain,
      solChain,
      cosmChain,
      readyChains,
      starknetChain,
      radixChain,
      aleoChain,
      tronChain,
      midnightChain,
    ],
  );
}
