import { httpClientProofProvider } from '@midnight-ntwrk/midnight-js-http-client-proof-provider';
import { indexerPublicDataProvider } from '@midnight-ntwrk/midnight-js-indexer-public-data-provider';
import { levelPrivateStateProvider } from '@midnight-ntwrk/midnight-js-level-private-state-provider';
import { NodeZkConfigProvider } from '@midnight-ntwrk/midnight-js-node-zk-config-provider';

import {
  waitForSync,
  type WalletContext,
  type WalletEndpoints,
} from './wallet.js';

export interface ProviderOptions {
  endpoints: WalletEndpoints;
  // Absolute path of the level DB directory. This must be midnightDbName —
  // the provider's privateStateStoreName option is only a sublevel name
  // inside the DB, and the DB itself would land in the process cwd.
  privateStateDbPath: string;
  privateStatePassword: string;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type ContractProviders = any;

/**
 * Build the provider bundle that `deployContract` / `findDeployedContract`
 * consume. `zkConfigPath` points to the per-contract artifacts dir so the
 * zk-config provider can locate prover/verifier keys.
 */
export async function createProviders(
  walletCtx: WalletContext,
  zkConfigPath: string,
  options: ProviderOptions,
): Promise<ContractProviders> {
  const state = await waitForSync(walletCtx);
  if (!state.shielded) {
    throw new Error('wallet state has no shielded sub-wallet');
  }
  const shielded = state.shielded;

  // CAST: `balanceTx` and `submitTx` straddle two SDK packages —
  // midnight-js-contracts types its provider's tx parameters against
  // classes that aren't nominally compatible with the wallet-sdk's. The
  // runtime contract holds; the types don't reconcile across packages.
  /* eslint-disable @typescript-eslint/no-explicit-any */
  const walletProvider = {
    getCoinPublicKey: () => shielded.coinPublicKey.toHexString(),
    getEncryptionPublicKey: () =>
      (
        state as unknown as {
          shielded: { encryptionPublicKey: { toHexString(): string } };
        }
      ).shielded.encryptionPublicKey.toHexString(),
    async balanceTx(tx: any, ttl?: Date) {
      const recipe = await walletCtx.wallet.balanceUnboundTransaction(
        tx,
        {
          shieldedSecretKeys: walletCtx.shieldedSecretKeys as any,
          dustSecretKey: walletCtx.dustSecretKey,
        },
        { ttl: ttl ?? new Date(Date.now() + 30 * 60 * 1000) },
      );
      // signRecipe signs the unshielded-offer inputs that any
      // `receiveUnshielded`-using circuit produces (e.g. `night.fund`).
      // Without it the chain rejects with
      // `MalformedError::InputsSignaturesLengthMismatch` (error 192).
      const signed = await walletCtx.wallet.signRecipe(
        recipe,
        (payload) => walletCtx.unshieldedKeystore.signDataAsync(payload) as any,
      );
      return walletCtx.wallet.finalizeRecipe(signed);
    },
    submitTx: (tx: any) => walletCtx.wallet.submitTransaction(tx),
  };
  /* eslint-enable @typescript-eslint/no-explicit-any */

  const zkConfigProvider = new NodeZkConfigProvider(zkConfigPath);

  return {
    privateStateProvider: levelPrivateStateProvider({
      privateStoragePasswordProvider: async () => options.privateStatePassword,
      accountId: String(walletCtx.unshieldedKeystore.getBech32Address()),
      midnightDbName: options.privateStateDbPath,
    }),
    publicDataProvider: indexerPublicDataProvider(
      options.endpoints.indexer,
      options.endpoints.indexerWs,
    ),
    zkConfigProvider,
    // The SDK's default /prove timeout is too tight for stagenet-sized
    // circuits on constrained hosts; overridable via PROOF_TIMEOUT_MS.
    proofProvider: httpClientProofProvider(
      options.endpoints.proofServer,
      zkConfigProvider,
      { timeout: Number(process.env.PROOF_TIMEOUT_MS ?? 600000) },
    ),
    walletProvider,
    midnightProvider: walletProvider,
  };
}
