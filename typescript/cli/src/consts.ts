import { type MinimumRequiredGasByAction } from '@hyperlane-xyz/provider-sdk';

export const ETHEREUM_MINIMUM_GAS: MinimumRequiredGasByAction = {
  CORE_DEPLOY_GAS: BigInt(1e8),
  WARP_DEPLOY_GAS: BigInt(3e7),
  ISM_DEPLOY_GAS: BigInt(5e5),
  HOOK_DEPLOY_GAS: BigInt(5e5),
  TEST_SEND_GAS: BigInt(3e5),
  AVS_GAS: BigInt(3e6),
};

export const PROXY_DEPLOYED_URL = 'https://proxy.hyperlane.xyz';
export const EXPLORER_URL = 'https://explorer.hyperlane.xyz';

// The Midnight registry pinned to a commit of its `midnight` branch, so a
// branch move cannot change the addresses a command resolves.
export const MIDNIGHT_REGISTRY_URI =
  'https://github.com/equilibriumco/hyperlane-registry/tree/ca458c7e01b31f2500278537e468fde138ef5428';
