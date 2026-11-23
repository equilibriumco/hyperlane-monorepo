import { expect } from 'chai';
import sinon from 'sinon';

import { MidnightWarpArtifactManager } from '@hyperlane-xyz/midnight-sdk';
import {
  EvmWarpRouteReader,
  MultiProvider,
  TokenStandard,
  TokenType,
  type WarpRouteDeployConfig,
} from '@hyperlane-xyz/sdk';
import { ProtocolType } from '@hyperlane-xyz/utils';

import { assertRouteMatchesNight } from './midnightRoute.js';

const NIGHT = '5a'.repeat(32);
const SNIGHT = '0x4eEb89B1061AA22a3FD2B3DB20914dBf50D76753';

const MIDNIGHT_METADATA = {
  name: 'midnight1',
  protocol: ProtocolType.Midnight,
  domainId: 1234,
  chainId: 1234,
  nativeToken: { decimals: 6, symbol: 'NIGHT', name: 'NIGHT' },
  rpcUrls: [{ http: 'http://127.0.0.1:9944' }],
  gatewayUrls: [{ http: 'http://127.0.0.1:8088/api/v3/graphql' }],
};

function contextFor(chains: Record<string, ProtocolType>) {
  const multiProvider = sinon.createStubInstance(MultiProvider);
  multiProvider.getProtocol.callsFake((chain) => chains[String(chain)]);
  multiProvider.getChainMetadata.returns(MIDNIGHT_METADATA as any);
  multiProvider.tryGetEvmExplorerMetadataList.returns([]);
  return {
    multiProvider,
    registry: { getChainAddresses: async () => ({ mailbox: NIGHT }) },
  } as any;
}

describe('assertRouteMatchesNight', () => {
  let readNight: sinon.SinonStub;
  let readToken: sinon.SinonStub;
  let context: ReturnType<typeof contextFor>;

  beforeEach(() => {
    context = contextFor({
      midnight1: ProtocolType.Midnight,
      sepolia: ProtocolType.Ethereum,
    });
    readNight = sinon
      .stub(MidnightWarpArtifactManager.prototype, 'readWarpToken')
      .resolves({ config: { decimals: 6, scale: 1e12 } } as any);
    sinon
      .stub(EvmWarpRouteReader.prototype, 'deriveTokenType')
      .resolves(TokenType.synthetic);
    readToken = sinon
      .stub(EvmWarpRouteReader.prototype, 'fetchTokenConfig')
      .resolves({ decimals: 18, scale: undefined } as any);
  });

  afterEach(() => sinon.restore());

  // warp deploy: night is reference data, the synthetic is still a config.
  const deployConfig = (decimals: number): WarpRouteDeployConfig =>
    ({
      midnight1: {
        type: TokenType.native,
        decimals: 6,
        scale: 1e12,
        foreignDeployment: NIGHT,
      },
      sepolia: { type: TokenType.synthetic, decimals, scale: 1 },
    }) as any;

  it('takes a new token whose decimals give the message amounts night uses', async () => {
    await assertRouteMatchesNight(context, deployConfig(18));
    expect(readNight.calledOnceWith(NIGHT)).to.equal(true);
    expect(readToken.called).to.equal(false);
  });

  it('refuses a new token that would scale amounts differently from night', async () => {
    const error = await assertRouteMatchesNight(context, deployConfig(6)).then(
      () => undefined,
      (err: Error) => err.message,
    );
    expect(error).to.include(
      `midnight1: 6 decimals, scale 1000000000000 (night at ${NIGHT}); ` +
        'sepolia: 6 decimals, scale 1 (the route config)',
    );
  });

  // warp apply with the roles swapped: night is enrolling a token that exists.
  const applyConfig = {
    midnight1: { type: TokenType.native, decimals: 6, scale: 1e12 },
    sepolia: { type: TokenType.synthetic, foreignDeployment: SNIGHT },
  } as any;
  const warpCoreConfig = {
    tokens: [
      {
        chainName: 'midnight1',
        standard: TokenStandard.EvmHypSynthetic,
        decimals: 6,
        symbol: 'NIGHT',
        name: 'NIGHT',
        addressOrDenom: NIGHT,
      },
    ],
  } as any;

  it('reads a token that exists on chain, not its config', async () => {
    await assertRouteMatchesNight(context, applyConfig, warpCoreConfig);
    expect(readToken.calledOnceWith(TokenType.synthetic, SNIGHT)).to.equal(
      true,
    );

    readToken.resolves({ decimals: 6, scale: undefined } as any);
    const error = await assertRouteMatchesNight(
      context,
      applyConfig,
      warpCoreConfig,
    ).then(
      () => undefined,
      (err: Error) => err.message,
    );
    expect(error).to.include(
      `sepolia: 6 decimals, scale 1 (the token at ${SNIGHT})`,
    );
  });

  it('reads nothing for a route without a Midnight chain', async () => {
    await assertRouteMatchesNight(
      contextFor({
        anvil2: ProtocolType.Ethereum,
        sepolia: ProtocolType.Ethereum,
      }),
      {
        anvil2: { type: TokenType.synthetic },
        sepolia: { type: TokenType.synthetic },
      } as any,
    );
    expect(readNight.called).to.equal(false);
    expect(readToken.called).to.equal(false);
  });
});
