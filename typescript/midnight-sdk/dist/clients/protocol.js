import { MidnightProvider } from './provider.js';
export class MidnightProtocolProvider {
    createProvider(chainMetadata) {
        return MidnightProvider.connect(chainMetadata);
    }
    async createSigner(_chainMetadata, _config) {
        throw new Error('MidnightProtocolProvider.createSigner: not implemented yet (#105)');
    }
    createSubmitter(_chainMetadata, _config) {
        throw new Error('MidnightProtocolProvider.createSubmitter: not implemented yet (#105)');
    }
    createIsmArtifactManager(_chainMetadata) {
        throw new Error('MidnightProtocolProvider.createIsmArtifactManager: not implemented yet (#105)');
    }
    createHookArtifactManager(_chainMetadata, _context) {
        throw new Error('MidnightProtocolProvider.createHookArtifactManager: not implemented yet (#105)');
    }
    createWarpArtifactManager(_chainMetadata, _context) {
        throw new Error('MidnightProtocolProvider.createWarpArtifactManager: not implemented yet (#105)');
    }
    createMailboxArtifactManager(_chainMetadata) {
        throw new Error('MidnightProtocolProvider.createMailboxArtifactManager: not implemented yet (#105)');
    }
    createValidatorAnnounceArtifactManager(_chainMetadata) {
        throw new Error('MidnightProtocolProvider.createValidatorAnnounceArtifactManager: not implemented yet (#105)');
    }
    createFeeArtifactManager(_chainMetadata, _context) {
        return null;
    }
    getMinGas() {
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
//# sourceMappingURL=protocol.js.map