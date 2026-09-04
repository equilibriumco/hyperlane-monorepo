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
  // Absolute path of the level DB directory, which must be `midnightDbName`:
  // `privateStateStoreName` only names a sublevel inside the DB, so the DB
  // itself would otherwise land in the process cwd.
  privateStateDbPath: string;
  privateStatePassword: string;
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export type ContractProviders = any;

/**
 * Build the provider bundle `deployContract` and `findDeployedContract`
 * consume. `zkConfigPath` is the per-contract artifacts dir holding the
 * prover/verifier keys.
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

  // `balanceTx` and `submitTx` straddle two SDK packages whose tx types are not
  // nominally compatible. The runtime shapes match; the types do not.
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
          shieldedSecretKeys: walletCtx.shieldedSecretKeys,
          dustSecretKey: walletCtx.dustSecretKey,
        },
        { ttl: ttl ?? new Date(Date.now() + 30 * 60 * 1000) },
      );
      // Signs the unshielded-offer inputs any `receiveUnshielded` circuit
      // produces; without it the chain rejects with error 192.
      const signed = await walletCtx.wallet.signRecipe(recipe, (payload) =>
        walletCtx.unshieldedKeystore.signDataAsync(payload),
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
