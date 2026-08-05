/**
 * Deployer wallet construction, ported from hyperlane-midnight. Builds the
 * WalletFacade (shielded + unshielded + dust sub-wallets) from an HD seed;
 * the unshielded keystore signs unshielded-offer inputs and pays fees in
 * DUST generated from registered NIGHT UTXOs.
 */
import * as Rx from 'rxjs';
import { WebSocket } from 'ws';

import { getNetworkId } from '@midnight-ntwrk/midnight-js-network-id';
import * as ledger from '@midnightntwrk/ledger-v9';
import {
  DustWallet,
  HDWallet,
  InMemoryTransactionHistoryStorage,
  PublicKey,
  Roles,
  ShieldedWallet,
  UnshieldedWallet,
  WalletEntrySchema,
  WalletFacade,
  createKeystore,
  mergeWalletEntries,
} from '@midnightntwrk/wallet-sdk';
import type { UnshieldedAddress } from '@midnightntwrk/wallet-sdk';

// Wallet SDK uses a global WebSocket to drive indexer GraphQL
// subscriptions; Node has no DOM-style WebSocket so we polyfill from `ws`.
// @ts-expect-error — DOM WebSocket vs `ws` types differ but the runtime contract holds.
globalThis.WebSocket ??= WebSocket;

export interface WalletEndpoints {
  indexer: string;
  indexerWs: string;
  node: string;
  proofServer: string;
}

export type WalletContext = Awaited<ReturnType<typeof createWallet>>;

export function deriveKeys(seedHex: string) {
  const seed = Buffer.from(seedHex, 'hex');
  const hd = HDWallet.fromSeed(seed);
  if (hd.type !== 'seedOk') {
    throw new Error(`Invalid seed: ${hd.type}`);
  }
  const result = hd.hdWallet
    .selectAccount(0)
    .selectRoles([Roles.Zswap, Roles.NightExternal, Roles.Dust])
    .deriveKeysAt(0);
  if (result.type !== 'keysDerived') {
    throw new Error(`Key derivation failed: ${result.type}`);
  }
  hd.hdWallet.clear();
  return result.keys;
}

/**
 * Derive the unshielded (NightExternal role) keystore alone — enough for a
 * synchronous signer address without starting or syncing a full wallet.
 * `setNetworkId` must have been called first.
 */
export function deriveUnshieldedKeystore(seedHex: string) {
  const keys = deriveKeys(seedHex);
  return createKeystore(
    { kind: 'schnorr', secret: keys[Roles.NightExternal] },
    getNetworkId(),
  );
}

export async function createWallet(
  seedHex: string,
  endpoints: WalletEndpoints,
) {
  const keys = deriveKeys(seedHex);
  const networkId = getNetworkId();

  const shieldedSecretKeys = ledger.ZswapSecretKeys.fromSeed(keys[Roles.Zswap]);
  const dustSecretKey = ledger.DustSecretKey.fromSeed(keys[Roles.Dust]);
  const unshieldedKeystore = createKeystore(
    { kind: 'schnorr', secret: keys[Roles.NightExternal] },
    networkId,
  );

  // wallet-sdk beta.2 reads `configuration.txHistoryStorage` at the FACADE
  // level; without it post-submit bookkeeping throws. Shared with the
  // unshielded sub-wallet below.
  const txHistoryStorage = new InMemoryTransactionHistoryStorage(
    WalletEntrySchema,
    mergeWalletEntries,
  );

  const walletConfig = {
    networkId,
    indexerClientConnection: {
      indexerHttpUrl: endpoints.indexer,
      indexerWsUrl: endpoints.indexerWs,
    },
    provingServerUrl: new URL(endpoints.proofServer),
    relayURL: new URL(endpoints.node.replace(/^http/, 'ws')),
    txHistoryStorage,
    // CAST: `DefaultConfiguration` requires fields the SDK accepts as a
    // partial structure at runtime via per-sub-wallet configuration
    // callbacks.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
  } as any;

  const wallet = await WalletFacade.init({
    configuration: walletConfig,
    // CASTs below bridge nested SDK-internal copies of types that don't
    // reconcile nominally with the top-level exports; one published SDK,
    // identical runtime shapes.
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    shielded: (config) =>
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      ShieldedWallet(config).startWithSecretKeys(
        shieldedSecretKeys as any,
      ) as any,
    unshielded: (config) => {
      const unshielded = UnshieldedWallet({
        networkId,
        indexerClientConnection: config.indexerClientConnection,
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        txHistoryStorage: txHistoryStorage as any,
      }).startWithPublicKey(PublicKey.fromKeyStore(unshieldedKeystore));
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      return unshielded as any;
    },
    dust: (config) =>
      DustWallet({
        ...config,
        // Fee headroom defaults; tx generation hits these when DUST is
        // tight on a fresh wallet.
        costParameters: {
          additionalFeeOverhead: 300_000_000_000_000n,
          feeBlocksMargin: 5,
        },
      }).startWithSecretKey(
        dustSecretKey,
        ledger.LedgerParameters.initialParameters().dust,
      ),
  });

  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  await wallet.start(shieldedSecretKeys as any, dustSecretKey as any);

  return { wallet, shieldedSecretKeys, dustSecretKey, unshieldedKeystore };
}

export async function closeWallet(ctx: WalletContext): Promise<void> {
  await ctx.wallet.stop();
}

// WalletFacade.state() emits a complex generic type the wallet-sdk doesn't
// export cleanly; treat the state object as a structural subset.
export interface WalletState {
  isSynced: boolean;
  shielded?: { coinPublicKey: { toHexString(): string } };
  unshielded?: {
    balances: Record<string, bigint>;
    availableCoins: ReadonlyArray<{
      meta?: { registeredForDustGeneration?: boolean };
    }>;
  };
  dust?: { balance(now: Date): bigint };
}

export async function waitForSync(ctx: WalletContext): Promise<WalletState> {
  return await Rx.firstValueFrom(
    ctx.wallet.state().pipe(Rx.filter((s: WalletState) => s.isSynced)),
  );
}

export async function syncedCoinPublicKeyHex(
  ctx: WalletContext,
): Promise<string> {
  const state = await waitForSync(ctx);
  if (!state.shielded) {
    throw new Error('wallet state has no shielded sub-wallet');
  }
  return state.shielded.coinPublicKey.toHexString();
}

export async function unshieldedNightBalance(
  ctx: WalletContext,
): Promise<bigint> {
  const state = await waitForSync(ctx);
  return state.unshielded?.balances[ledger.nativeToken().raw] ?? 0n;
}

export async function getUnshieldedAddress(
  ctx: WalletContext,
): Promise<UnshieldedAddress> {
  // CAST: getAddress() returns the dust-wallet-nested address-format copy;
  // our public type is the top-level copy. Same class, distinct bundled copy.
  return (await ctx.wallet.unshielded.getAddress()) as unknown as UnshieldedAddress;
}

async function waitForDust(ctx: WalletContext): Promise<bigint> {
  return await Rx.firstValueFrom(
    ctx.wallet.state().pipe(
      Rx.throttleTime(5000),
      Rx.filter((s: WalletState) => s.isSynced),
      Rx.map((s: WalletState) => s.dust?.balance(new Date()) ?? 0n),
      Rx.filter((b: bigint) => b > 0n),
    ),
  );
}

/**
 * Register the wallet's unregistered NIGHT UTXOs for DUST generation.
 * Idempotent. Returns once DUST balance is positive so the caller can
 * immediately submit fee-paying transactions.
 */
export async function registerNightForDust(ctx: WalletContext): Promise<void> {
  const state = await waitForSync(ctx);

  const unregistered = (state.unshielded?.availableCoins ?? []).filter(
    (coin) => coin.meta?.registeredForDustGeneration === false,
  );

  if (unregistered.length > 0) {
    const recipe = await ctx.wallet.registerNightUtxosForDustGeneration(
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      unregistered as any,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      ctx.unshieldedKeystore.getPublicKey() as any,
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      (payload: Uint8Array) =>
        ctx.unshieldedKeystore.signDataAsync(payload) as any,
    );
    const finalized = await ctx.wallet.finalizeRecipe(recipe);
    await ctx.wallet.submitTransaction(finalized);
  }

  await waitForDust(ctx);
}
