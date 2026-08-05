import type {
  AltVM,
  ChainMetadataForAltVM,
  ITransactionSubmitter,
  MinimumRequiredGasByAction,
  ProtocolProvider,
  SignerConfig,
  TransactionSubmitterConfig,
} from '@hyperlane-xyz/provider-sdk';
import type { IProvider } from '@hyperlane-xyz/provider-sdk/altvm';
import type {
  FeeReadContext,
  IRawFeeArtifactManager,
} from '@hyperlane-xyz/provider-sdk/fee';
import type { IRawHookArtifactManager } from '@hyperlane-xyz/provider-sdk/hook';
import type { IRawIsmArtifactManager } from '@hyperlane-xyz/provider-sdk/ism';
import type { IRawMailboxArtifactManager } from '@hyperlane-xyz/provider-sdk/mailbox';
import type {
  AnnotatedTx,
  TxReceipt,
} from '@hyperlane-xyz/provider-sdk/module';
import type { IRawValidatorAnnounceArtifactManager } from '@hyperlane-xyz/provider-sdk/validator-announce';
import type { IRawWarpArtifactManager } from '@hyperlane-xyz/provider-sdk/warp';

import { MidnightProvider } from './provider.js';

export class MidnightProtocolProvider implements ProtocolProvider {
  createProvider(chainMetadata: ChainMetadataForAltVM): Promise<IProvider> {
    return MidnightProvider.connect(chainMetadata);
  }

  async createSigner(
    _chainMetadata: ChainMetadataForAltVM,
    _config: SignerConfig,
  ): Promise<AltVM.ISigner<AnnotatedTx, TxReceipt>> {
    throw new Error(
      'MidnightProtocolProvider.createSigner: not implemented yet (#105)',
    );
  }

  createSubmitter<TConfig extends TransactionSubmitterConfig>(
    _chainMetadata: ChainMetadataForAltVM,
    _config: TConfig,
  ): Promise<ITransactionSubmitter> {
    throw new Error(
      'MidnightProtocolProvider.createSubmitter: not implemented yet (#105)',
    );
  }

  createIsmArtifactManager(
    _chainMetadata: ChainMetadataForAltVM,
  ): IRawIsmArtifactManager {
    throw new Error(
      'MidnightProtocolProvider.createIsmArtifactManager: not implemented yet (#105)',
    );
  }

  createHookArtifactManager(
    _chainMetadata: ChainMetadataForAltVM,
    _context?: { mailbox?: string },
  ): IRawHookArtifactManager {
    throw new Error(
      'MidnightProtocolProvider.createHookArtifactManager: not implemented yet (#105)',
    );
  }

  createWarpArtifactManager(
    _chainMetadata: ChainMetadataForAltVM,
    _context?: { mailbox?: string },
  ): IRawWarpArtifactManager {
    throw new Error(
      'MidnightProtocolProvider.createWarpArtifactManager: not implemented yet (#105)',
    );
  }

  createMailboxArtifactManager(
    _chainMetadata: ChainMetadataForAltVM,
  ): IRawMailboxArtifactManager {
    throw new Error(
      'MidnightProtocolProvider.createMailboxArtifactManager: not implemented yet (#105)',
    );
  }

  createValidatorAnnounceArtifactManager(
    _chainMetadata: ChainMetadataForAltVM,
  ): IRawValidatorAnnounceArtifactManager | null {
    throw new Error(
      'MidnightProtocolProvider.createValidatorAnnounceArtifactManager: not implemented yet (#105)',
    );
  }

  createFeeArtifactManager(
    _chainMetadata: ChainMetadataForAltVM,
    _context: FeeReadContext,
  ): IRawFeeArtifactManager | null {
    return null;
  }

  getMinGas(): MinimumRequiredGasByAction {
    // Midnight has no gas market: transaction fees are paid in DUST and
    // computed by the wallet at balancing time, so there is no meaningful
    // native-token minimum to enforce per action.
    return {
      CORE_DEPLOY_GAS: 0n,
      WARP_DEPLOY_GAS: 0n,
      TEST_SEND_GAS: 0n,
      AVS_GAS: 0n,
      ISM_DEPLOY_GAS: 0n,
      HOOK_DEPLOY_GAS: 0n,
    };
  }
}
