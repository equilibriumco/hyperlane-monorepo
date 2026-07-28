import { useCallback, useMemo } from 'react';

import type { MinimalProviderRegistry } from '@hyperlane-xyz/sdk/providers/MinimalProviderRegistry';
import type { MultiProviderAdapter } from '@hyperlane-xyz/sdk/providers/MultiProviderAdapter';
import { ProtocolType } from '@hyperlane-xyz/utils';

import type {
  AccountInfo,
  ActiveChainInfo,
  ChainTransactionFns,
  SwitchNetworkFns,
  WalletDetails,
  WatchAssetFns,
} from './types.js';

/**
 * Midnight has no browser wallet integration yet (transactions require a
 * proof-server pipeline that browser wallets do not provide). These hooks
 * satisfy the per-protocol wallet maps with inert values; any attempt to
 * actually transact fails loudly.
 */

const NO_WALLET_MSG =
  'Midnight has no wallet integration yet; transactions must be submitted through a Midnight proof-server pipeline';

export function useMidnightAccount(
  _multiProvider: MinimalProviderRegistry,
): AccountInfo {
  return useMemo<AccountInfo>(
    () => ({
      protocol: ProtocolType.Midnight,
      addresses: [],
      publicKey: undefined,
      isReady: false,
    }),
    [],
  );
}

export function useMidnightWalletDetails(): WalletDetails {
  return useMemo<WalletDetails>(
    () => ({ name: undefined, logoUrl: undefined }),
    [],
  );
}

export function useMidnightConnectFn(): () => void {
  return useCallback(() => {
    throw new Error(NO_WALLET_MSG);
  }, []);
}

export function useMidnightDisconnectFn(): () => Promise<void> {
  return useCallback(async () => {}, []);
}

export function useMidnightActiveChain(
  _multiProvider: MinimalProviderRegistry,
): ActiveChainInfo {
  return useMemo<ActiveChainInfo>(() => ({}), []);
}

export function useMidnightSwitchNetwork(
  _multiProvider: MultiProviderAdapter,
): SwitchNetworkFns {
  const switchNetwork = useCallback(async (_chainName: string) => {
    throw new Error(NO_WALLET_MSG);
  }, []);
  return { switchNetwork };
}

export function useMidnightWatchAsset(
  _multiProvider: MultiProviderAdapter,
): WatchAssetFns {
  const addAsset = useCallback(async () => {
    throw new Error(NO_WALLET_MSG);
  }, []);
  return { addAsset };
}

export function useMidnightTransactionFns(
  multiProvider: MultiProviderAdapter,
): ChainTransactionFns {
  const { switchNetwork } = useMidnightSwitchNetwork(multiProvider);

  const sendTransaction = useCallback(async () => {
    throw new Error(NO_WALLET_MSG);
  }, []);

  const sendMultiTransaction = useCallback(async () => {
    throw new Error(NO_WALLET_MSG);
  }, []);

  return { sendTransaction, sendMultiTransaction, switchNetwork };
}
