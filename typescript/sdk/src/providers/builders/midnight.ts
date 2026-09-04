import { MidnightProvider as MidnightSDKProvider } from '@hyperlane-xyz/midnight-sdk/runtime';

import type { ChainMetadata } from '../../metadata/chainMetadataTypes.js';
import type { MidnightProvider } from '../ProviderType.js';
import { ProviderType } from '../ProviderType.js';

import type { ProviderBuilderFn } from './types.js';

/**
 * Midnight has no JSON-RPC interface: the read provider runs contract circuits
 * locally against state fetched from the GraphQL indexer in `gatewayUrls`.
 */
export const defaultMidnightProviderBuilder: ProviderBuilderFn<
  MidnightProvider
> = (metadata: ChainMetadata) => {
  return {
    provider: MidnightSDKProvider.fromMetadata(metadata),
    type: ProviderType.Midnight,
  };
};
