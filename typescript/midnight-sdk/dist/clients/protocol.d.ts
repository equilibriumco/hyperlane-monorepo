import type { AltVM, ChainMetadataForAltVM, ITransactionSubmitter, MinimumRequiredGasByAction, ProtocolProvider, SignerConfig, TransactionSubmitterConfig } from '@hyperlane-xyz/provider-sdk';
import type { IProvider } from '@hyperlane-xyz/provider-sdk/altvm';
import type { FeeReadContext, IRawFeeArtifactManager } from '@hyperlane-xyz/provider-sdk/fee';
import type { IRawHookArtifactManager } from '@hyperlane-xyz/provider-sdk/hook';
import type { IRawIsmArtifactManager } from '@hyperlane-xyz/provider-sdk/ism';
import type { IRawMailboxArtifactManager } from '@hyperlane-xyz/provider-sdk/mailbox';
import type { AnnotatedTx, TxReceipt } from '@hyperlane-xyz/provider-sdk/module';
import type { IRawValidatorAnnounceArtifactManager } from '@hyperlane-xyz/provider-sdk/validator-announce';
import type { IRawWarpArtifactManager } from '@hyperlane-xyz/provider-sdk/warp';
export declare class MidnightProtocolProvider implements ProtocolProvider {
    createProvider(chainMetadata: ChainMetadataForAltVM): Promise<IProvider>;
    createSigner(_chainMetadata: ChainMetadataForAltVM, _config: SignerConfig): Promise<AltVM.ISigner<AnnotatedTx, TxReceipt>>;
    createSubmitter<TConfig extends TransactionSubmitterConfig>(_chainMetadata: ChainMetadataForAltVM, _config: TConfig): Promise<ITransactionSubmitter>;
    createIsmArtifactManager(_chainMetadata: ChainMetadataForAltVM): IRawIsmArtifactManager;
    createHookArtifactManager(_chainMetadata: ChainMetadataForAltVM, _context?: {
        mailbox?: string;
    }): IRawHookArtifactManager;
    createWarpArtifactManager(_chainMetadata: ChainMetadataForAltVM, _context?: {
        mailbox?: string;
    }): IRawWarpArtifactManager;
    createMailboxArtifactManager(_chainMetadata: ChainMetadataForAltVM): IRawMailboxArtifactManager;
    createValidatorAnnounceArtifactManager(_chainMetadata: ChainMetadataForAltVM): IRawValidatorAnnounceArtifactManager | null;
    createFeeArtifactManager(_chainMetadata: ChainMetadataForAltVM, _context: FeeReadContext): IRawFeeArtifactManager | null;
    getMinGas(): MinimumRequiredGasByAction;
}
//# sourceMappingURL=protocol.d.ts.map