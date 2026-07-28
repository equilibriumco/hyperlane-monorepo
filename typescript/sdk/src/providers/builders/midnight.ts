import { assert } from '@hyperlane-xyz/utils';

import type { ChainMetadata } from '../../metadata/chainMetadataTypes.js';
import type { MidnightProvider } from '../ProviderType.js';
import { ProviderType } from '../ProviderType.js';

import type { ProviderBuilderFn } from './types.js';

/**
 * Midnight has no JSON-RPC interface; the chain is read through its GraphQL
 * indexer. The provider therefore only carries the indexer endpoint from the
 * chain metadata — construction never dials the network.
 */
export const defaultMidnightProviderBuilder: ProviderBuilderFn<
  MidnightProvider
> = (metadata: ChainMetadata) => {
  const { rpcUrls } = metadata;
  assert(
    rpcUrls.length > 0,
    'Midnight requires at least one rpcUrl (the indexer GraphQL endpoint)',
  );
  return {
    provider: { url: rpcUrls[0].http },
    type: ProviderType.Midnight,
  };
};
