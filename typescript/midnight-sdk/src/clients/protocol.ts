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

import { MidnightHookArtifactManager } from '../hook/hook-artifact-manager.js';
import { MidnightIsmArtifactManager } from '../ism/ism-artifact-manager.js';
import { MidnightMailboxArtifactManager } from '../mailbox/mailbox-artifact-manager.js';
import { MidnightValidatorAnnounceArtifactManager } from '../validator-announce/validator-announce-artifact-manager.js';
import { MidnightWarpArtifactManager } from '../warp/warp-artifact-manager.js';

import { MidnightProvider } from './provider.js';
import { MidnightSigner } from './signer.js';

export class MidnightProtocolProvider implements ProtocolProvider {
  createProvider(chainMetadata: ChainMetadataForAltVM): Promise<IProvider> {
    return MidnightProvider.connect(chainMetadata);
  }

  async createSigner(
    chainMetadata: ChainMetadataForAltVM,
    config: SignerConfig,
  ): Promise<AltVM.ISigner<AnnotatedTx, TxReceipt>> {
    return MidnightSigner.connectWithSigner(chainMetadata, config.privateKey);
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
    chainMetadata: ChainMetadataForAltVM,
  ): IRawIsmArtifactManager {
    return new MidnightIsmArtifactManager(chainMetadata);
  }

  createHookArtifactManager(
    chainMetadata: ChainMetadataForAltVM,
    context?: { mailbox?: string },
  ): IRawHookArtifactManager {
    return new MidnightHookArtifactManager(chainMetadata, context);
  }

  createWarpArtifactManager(
    chainMetadata: ChainMetadataForAltVM,
    _context?: { mailbox?: string },
  ): IRawWarpArtifactManager {
    return new MidnightWarpArtifactManager(chainMetadata);
  }

  createMailboxArtifactManager(
    chainMetadata: ChainMetadataForAltVM,
  ): IRawMailboxArtifactManager {
    return new MidnightMailboxArtifactManager(chainMetadata);
  }

  createValidatorAnnounceArtifactManager(
    chainMetadata: ChainMetadataForAltVM,
  ): IRawValidatorAnnounceArtifactManager | null {
    return new MidnightValidatorAnnounceArtifactManager(chainMetadata);
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
