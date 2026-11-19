import { httpClientProofProvider } from '@midnight-ntwrk/midnight-js-http-client-proof-provider';
import { indexerPublicDataProvider } from '@midnight-ntwrk/midnight-js-indexer-public-data-provider';
import { levelPrivateStateProvider } from '@midnight-ntwrk/midnight-js-level-private-state-provider';
import { NodeZkConfigProvider } from '@midnight-ntwrk/midnight-js-node-zk-config-provider';
import {
  createLedger9MidnightProvider,
  createLedger9WalletProvider,
  type MidnightProviders,
} from '@midnight-ntwrk/midnight-js-types';

import {
  waitForSync,
  type WalletContext,
  type WalletEndpoints,
} from './wallet.js';

// How long a built transaction stays valid, the framework's and the wallet's
// part alike.
const TX_TTL_SECONDS = 3600;

export interface ProviderOptions {
  // The Midnight network transactions are built for.
  networkId: string;
  endpoints: WalletEndpoints;
  // Absolute path of the level DB directory, which must be `midnightDbName`:
  // `privateStateStoreName` only names a sublevel inside the DB, so the DB
  // itself would otherwise land in the process cwd.
  privateStateDbPath: string;
  privateStatePassword: () => string;
}

export type ContractProviders = MidnightProviders;

/**
 * Build the provider bundle `deployContract` and `findDeployedContract`
 * consume. `zkConfigPath` is the per-contract artifacts dir holding the
 * prover/verifier keys and the compiler's manifest they are checked against.
 */
export async function createProviders(
  walletCtx: WalletContext,
  zkConfigPath: string,
  options: ProviderOptions,
): Promise<ContractProviders> {
  // Read now, before anything is sent: a deploy first writes the store after
  // its transaction lands.
  const privateStatePassword = options.privateStatePassword();
  const { wallet, unshieldedKeystore } = walletCtx;
  const { shielded } = await waitForSync(walletCtx);

  // The wallet seals transactions in its own handles, so they cross as bytes.
  const walletProvider = createLedger9WalletProvider({
    getCoinPublicKey: () => shielded.coinPublicKey.toHexString(),
    getEncryptionPublicKey: () => shielded.encryptionPublicKey.toHexString(),
    async balanceTx(tx, ttl) {
      const recipe = await wallet.balanceUnboundTransaction(
        wallet.adoptTransaction(tx.serialize(), 'Unbound'),
        { ttl: ttl ?? new Date(Date.now() + TX_TTL_SECONDS * 1000) },
      );
      // Signs the unshielded-offer inputs any `receiveUnshielded` circuit
      // produces; without it the chain rejects with error 192.
      const signed = await wallet.signRecipe(recipe, (payload) =>
        unshieldedKeystore.signDataAsync(payload),
      );
      return (await wallet.finalizeRecipe(signed)).serialize();
    },
  });
  const midnightProvider = createLedger9MidnightProvider((tx) =>
    wallet.submitTransaction(
      wallet.adoptTransaction(tx.serialize(), 'Finalized'),
    ),
  );

  const zkConfigProvider = new NodeZkConfigProvider(zkConfigPath);

  return {
    privateStateProvider: levelPrivateStateProvider({
      privateStoragePasswordProvider: async () => privateStatePassword,
      accountId: String(unshieldedKeystore.getBech32Address()),
      midnightDbName: options.privateStateDbPath,
    }),
    publicDataProvider: indexerPublicDataProvider({
      queryURL: options.endpoints.indexer,
      subscriptionURL: options.endpoints.indexerWs,
    }),
    zkConfigProvider,
    // The SDK's default /prove timeout is too tight for the larger circuits
    // on constrained hosts; overridable via PROOF_TIMEOUT_MS.
    proofProvider: httpClientProofProvider({
      url: options.endpoints.proofServer,
      zkConfigProvider,
      timeout: Number(process.env.PROOF_TIMEOUT_MS ?? 600000),
    }),
    walletProvider,
    midnightProvider,
    config: { networkId: options.networkId, ttlSeconds: TX_TTL_SECONDS },
  };
}
