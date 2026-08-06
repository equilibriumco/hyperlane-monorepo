import type { MultiProviderAdapter } from '../../providers/MultiProviderAdapter.js';
import { TokenStandard } from '../TokenStandard.js';

import type { IHypTokenAdapter } from './ITokenAdapter.js';
import {
  MidnightHypCollateralAdapter,
  MidnightHypNativeAdapter,
  MidnightHypSyntheticAdapter,
} from './MidnightTokenAdapter.js';
import {
  type HypTokenAdapterInput,
  hasChainMetadata,
} from './hypTokenAdapterUtils.js';

export function createMidnightHypAdapter(
  multiProvider: MultiProviderAdapter<{ mailbox?: string }>,
  token: HypTokenAdapterInput,
): IHypTokenAdapter<unknown> | undefined {
  const { standard, chainName, addressOrDenom } = token;

  if (!standard || !hasChainMetadata(multiProvider, chainName)) {
    return undefined;
  }

  switch (standard) {
    case TokenStandard.MidnightHypNative:
      return new MidnightHypNativeAdapter(chainName, multiProvider, {
        token: addressOrDenom,
      });
    case TokenStandard.MidnightHypCollateral:
      return new MidnightHypCollateralAdapter(chainName, multiProvider, {
        token: addressOrDenom,
      });
    case TokenStandard.MidnightHypSynthetic:
      return new MidnightHypSyntheticAdapter(chainName, multiProvider, {
        token: addressOrDenom,
      });
    default:
      return undefined;
  }
}
