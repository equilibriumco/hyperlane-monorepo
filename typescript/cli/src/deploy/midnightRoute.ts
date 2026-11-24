import { MidnightWarpArtifactManager } from '@hyperlane-xyz/midnight-sdk';
import {
  EvmWarpRouteReader,
  type TokenMetadata,
  type WarpCoreConfig,
  type WarpRouteDeployConfig,
  altVmChainLookup,
  verifyScale,
} from '@hyperlane-xyz/sdk';
import { ProtocolType, assert, isEVMLike } from '@hyperlane-xyz/utils';

import { type WriteCommandContext } from '../context/types.js';

export type RouteDecimals = Pick<TokenMetadata, 'decimals' | 'scale'> & {
  from: string;
};

/**
 * night turns every amount into the message's 18 decimals with the decimals
 * and scale sealed at its deploy. A token on another chain whose decimals and
 * scale give other message amounts would mint or release the wrong amounts,
 * so a route with a Midnight chain is checked against the live night, and
 * against every other token that already exists, before anything is sent.
 */
export async function assertRouteMatchesNight(
  context: WriteCommandContext,
  warpDeployConfig: WarpRouteDeployConfig,
  warpCoreConfig?: WarpCoreConfig,
): Promise<void> {
  const { multiProvider, registry } = context;
  const chains = Object.keys(warpDeployConfig);
  if (
    !chains.some(
      (chain) => multiProvider.getProtocol(chain) === ProtocolType.Midnight,
    )
  ) {
    return;
  }

  const route = new Map<string, RouteDecimals>();
  for (const chain of chains) {
    const config = warpDeployConfig[chain];
    const protocol = multiProvider.getProtocol(chain);
    const address =
      config.foreignDeployment ??
      warpCoreConfig?.tokens.find((token) => token.chainName === chain)
        ?.addressOrDenom;

    if (protocol === ProtocolType.Midnight) {
      // night is the chain's mailbox and its warp token.
      const night =
        address ??
        config.mailbox ??
        (await registry.getChainAddresses(chain))?.mailbox;
      assert(
        night,
        `No night address for ${chain}: no mailbox in the registry`,
      );
      const { config: live } = await new MidnightWarpArtifactManager(
        altVmChainLookup(multiProvider).getChainMetadata(chain),
      ).readWarpToken(night);
      route.set(chain, {
        decimals: live.decimals,
        scale: live.scale,
        from: `night at ${night}`,
      });
    } else if (address && isEVMLike(protocol)) {
      const reader = new EvmWarpRouteReader(multiProvider, chain);
      const live = await reader.fetchTokenConfig(
        await reader.deriveTokenType(address),
        address,
      );
      route.set(chain, {
        decimals: live.decimals,
        scale: live.scale,
        from: `the token at ${address}`,
      });
    } else {
      route.set(chain, {
        decimals: config.decimals,
        scale: config.scale,
        from: 'the route config',
      });
    }
  }

  assertRouteDecimals(route);
}

// Every token's 10^decimals x scale has to be the same, so that each turns
// its amounts into the same message amounts.
export function assertRouteDecimals(route: Map<string, RouteDecimals>): void {
  const described = [...route]
    .map(
      ([chain, { decimals, scale, from }]) =>
        `${chain}: ${decimals} decimals, scale ${formatScale(scale)} (${from})`,
    )
    .join('; ');
  assert(
    [...route.values()].every(({ decimals }) => decimals !== undefined),
    `Every token of a route with night needs its decimals: ${described}`,
  );
  assert(
    verifyScale(
      new Map(
        [...route].map(([chain, { decimals, scale }]) => [
          chain,
          { name: chain, symbol: chain, decimals, scale },
        ]),
      ),
    ),
    `The route's tokens do not give night's message amounts: ${described}. ` +
      'Each token has to have 10^decimals x scale equal to the others',
  );
}

function formatScale(scale: TokenMetadata['scale']): string {
  if (scale === undefined) return '1';
  if (typeof scale === 'number') return String(scale);
  return `${scale.numerator}/${scale.denominator}`;
}
