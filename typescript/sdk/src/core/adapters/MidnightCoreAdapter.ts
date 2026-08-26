import { Address, HexString, assert, pollAsync } from '@hyperlane-xyz/utils';

import { BaseMidnightAdapter } from '../../app/MultiProtocolApp.js';
import type { MultiProviderAdapter } from '../../providers/MultiProviderAdapter.js';
import {
  ProviderType,
  TypedTransactionReceipt,
} from '../../providers/ProviderType.js';
import { ChainName } from '../../types.js';

import { ICoreAdapter } from './types.js';

export class MidnightCoreAdapter
  extends BaseMidnightAdapter
  implements ICoreAdapter
{
  constructor(
    public readonly chainName: ChainName,
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    public readonly multiProvider: MultiProviderAdapter<any>,
    public readonly addresses: { mailbox: Address },
  ) {
    super(chainName, multiProvider, addresses);
  }

  // transferRemote returns the messageId as its circuit result and the
  // Midnight signer copies it (plus the destination domain) onto the
  // receipt — there is no dispatch log to parse.
  async extractMessageIds(
    sourceTx: TypedTransactionReceipt,
  ): Promise<Array<{ messageId: string; destination: ChainName }>> {
    if (sourceTx.type !== ProviderType.Midnight) {
      return [];
    }
    const { messageId, destinationDomainId } = sourceTx.receipt;
    if (!messageId || destinationDomainId === undefined) {
      this.logger.warn(
        'Midnight receipt carries no messageId; no message dispatched',
      );
      return [];
    }
    const destination = this.multiProvider.tryGetChainName(destinationDomainId);
    if (!destination) {
      this.logger.warn(`Unknown destination domain ${destinationDomainId}`);
      return [];
    }
    return [{ messageId, destination }];
  }

  async waitForMessageProcessed(
    messageId: HexString,
    destination: ChainName,
    delayMs?: number,
    maxAttempts?: number,
  ): Promise<boolean> {
    const provider = this.multiProvider.getMidnightProvider(destination);

    await pollAsync(
      async () => {
        this.logger.debug(`Checking if message ${messageId} was processed`);
        const delivered = await provider.isMessageDelivered({
          mailboxAddress: this.addresses.mailbox,
          messageId,
        });

        assert(delivered, `Message ${messageId} not yet processed`);

        this.logger.info(`Message ${messageId} was processed`);
        return delivered;
      },
      delayMs,
      maxAttempts,
    );

    return true;
  }

  async isDelivered(
    messageId: HexString,
    _blockTag?: string | number,
  ): Promise<boolean> {
    const provider = this.multiProvider.getMidnightProvider(this.chainName);
    return provider.isMessageDelivered({
      mailboxAddress: this.addresses.mailbox,
      messageId,
    });
  }
}
