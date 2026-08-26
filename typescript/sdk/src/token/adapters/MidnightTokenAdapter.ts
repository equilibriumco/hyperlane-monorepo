import type {
  MidnightProvider,
  MidnightTransaction,
} from '@hyperlane-xyz/midnight-sdk/runtime';
import {
  Address,
  Domain,
  Numberish,
  addressToBytes32,
  assert,
  strip0x,
} from '@hyperlane-xyz/utils';

import { BaseMidnightAdapter } from '../../app/MultiProtocolApp.js';
import type { MultiProviderAdapter } from '../../providers/MultiProviderAdapter.js';
import { ChainName } from '../../types.js';
import { TokenMetadata } from '../types.js';

import {
  IHypTokenAdapter,
  ITokenAdapter,
  InterchainGasQuote,
  QuoteTransferRemoteParams,
  TransferParams,
  TransferRemoteParams,
} from './ITokenAdapter.js';

export class MidnightTokenAdapter
  extends BaseMidnightAdapter
  implements ITokenAdapter<MidnightTransaction>
{
  protected provider: MidnightProvider;
  protected tokenAddress: string;

  constructor(
    public readonly chainName: ChainName,
    public readonly multiProvider: MultiProviderAdapter,
    public readonly addresses: { token: Address },
  ) {
    super(chainName, multiProvider, addresses);

    this.provider = this.getProvider();
    this.tokenAddress = addresses.token;
  }

  protected getDenom(): string {
    const { nativeToken } = this.multiProvider.getChainMetadata(this.chainName);
    assert(
      nativeToken?.denom,
      'Midnight chain metadata has no nativeToken.denom',
    );
    return nativeToken.denom;
  }

  async getBalance(address: Address): Promise<bigint> {
    return this.provider.getBalance({
      address,
      denom: this.getDenom(),
    });
  }

  async getMetadata(): Promise<TokenMetadata> {
    const { nativeToken } = this.multiProvider.getChainMetadata(this.chainName);
    assert(
      nativeToken,
      `Native token data is required for ${MidnightTokenAdapter.name}`,
    );

    return {
      name: nativeToken.name,
      symbol: nativeToken.symbol,
      decimals: nativeToken.decimals,
    };
  }

  async getMinimumTransferAmount(_recipient: Address): Promise<bigint> {
    return 0n;
  }

  async isApproveRequired(
    _owner: Address,
    _spender: Address,
    _weiAmountOrId: Numberish,
  ): Promise<boolean> {
    return false;
  }

  async isRevokeApprovalRequired(
    _owner: Address,
    _spender: Address,
  ): Promise<boolean> {
    return false;
  }

  async populateApproveTx(
    _params: TransferParams,
  ): Promise<MidnightTransaction> {
    throw new Error('Approve not required for Midnight tokens');
  }

  async populateTransferTx(
    _params: TransferParams,
  ): Promise<MidnightTransaction> {
    // Plain NIGHT transfers are wallet-level operations, not contract
    // circuit calls, so they cannot be expressed as a MidnightTransaction.
    throw new Error('Plain transfers are not supported on Midnight');
  }

  async getTotalSupply(): Promise<bigint | undefined> {
    return this.provider.getTotalSupply({
      denom: this.getDenom(),
    });
  }
}

export class MidnightNativeTokenAdapter extends MidnightTokenAdapter {}

export class BaseMidnightHypTokenAdapter
  extends MidnightTokenAdapter
  implements IHypTokenAdapter<MidnightTransaction>
{
  async getDomains(): Promise<Domain[]> {
    const { remoteRouters } = await this.provider.getRemoteRouters({
      tokenAddress: this.tokenAddress,
    });

    return remoteRouters.map((router) => router.receiverDomainId);
  }

  async getRouterAddress(domain: Domain): Promise<Buffer> {
    const { remoteRouters } = await this.provider.getRemoteRouters({
      tokenAddress: this.tokenAddress,
    });

    const router = remoteRouters.find(
      (router) => router.receiverDomainId === domain,
    );

    if (!router) {
      throw new Error(`Router with domain "${domain}" not found`);
    }

    return Buffer.from(strip0x(router.receiverAddress), 'hex');
  }

  async getAllRouters(): Promise<Array<{ domain: Domain; address: Buffer }>> {
    const { remoteRouters } = await this.provider.getRemoteRouters({
      tokenAddress: this.tokenAddress,
    });

    return remoteRouters.map((router) => ({
      domain: router.receiverDomainId,
      address: Buffer.from(strip0x(router.receiverAddress), 'hex'),
    }));
  }

  async getBridgedSupply(): Promise<bigint | undefined> {
    return this.provider.getBridgedSupply({
      tokenAddress: this.tokenAddress,
    });
  }

  // The IGP is a standalone contract the token does not reference on-chain;
  // its address arrives as customHook from the warp route's
  // igpTokenAddressOrDenom field.
  async quoteTransferRemoteGas({
    destination,
    customHook,
  }: QuoteTransferRemoteParams): Promise<InterchainGasQuote> {
    const { denom: addressOrDenom, amount } =
      await this.provider.quoteRemoteTransfer({
        tokenAddress: this.tokenAddress,
        destinationDomainId: destination,
        customHookAddress: customHook,
      });

    return {
      igpQuote: {
        addressOrDenom,
        amount,
      },
    };
  }

  async populateTransferRemoteTx(
    params: TransferRemoteParams,
  ): Promise<MidnightTransaction> {
    assert(params.fromAccountOwner, `no sender in remote transfer params`);

    if (!params.interchainGas) {
      params.interchainGas = await this.quoteTransferRemoteGas({
        destination: params.destination,
        customHook: params.customHook,
      });
    }

    const igpQuote = params.interchainGas.igpQuote;
    assert(
      igpQuote?.addressOrDenom,
      `no denom in the interchainGas quote for max fee`,
    );
    assert(
      params.customHook,
      'Midnight remote transfers need the IGP address as customHook ' +
        '(set igpTokenAddressOrDenom on the warp route token)',
    );

    return this.provider.getRemoteTransferTransaction({
      signer: params.fromAccountOwner,
      tokenAddress: this.tokenAddress,
      destinationDomainId: params.destination,
      recipient: strip0x(addressToBytes32(params.recipient)),
      amount: params.weiAmountOrId.toString(),
      customHookAddress: params.customHook,
      gasLimit: '',
      maxFee: {
        denom: igpQuote.addressOrDenom,
        amount: igpQuote.amount.toString(),
      },
    });
  }
}

export class MidnightHypCollateralAdapter extends BaseMidnightHypTokenAdapter {}
export class MidnightHypSyntheticAdapter extends BaseMidnightHypTokenAdapter {}
export class MidnightHypNativeAdapter extends BaseMidnightHypTokenAdapter {}
