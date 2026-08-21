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

/**
 * The dust sub-wallet adds this to every fee it computes (`costParameters`
 * below), so a wallet holding less DUST than this cannot balance any
 * transaction. It is a floor, not a fee estimate: a large deploy needs more,
 * and MIDNIGHT_MIN_DUST_SPECKS raises the bar.
 */
const DUST_FEE_OVERHEAD_SPECKS = 300_000_000_000_000n;

/** DUST a wallet must hold before a transaction can be balanced. */
export function minimumDustSpecks(): bigint {
  const raw = process.env.MIDNIGHT_MIN_DUST_SPECKS;
  if (!raw) return DUST_FEE_OVERHEAD_SPECKS;
  if (!/^\d+$/.test(raw)) {
    throw new Error(
      `MIDNIGHT_MIN_DUST_SPECKS must be a whole number of specks, got "${raw}"`,
    );
  }
  return BigInt(raw);
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
        txHistoryStorage,
      }).startWithPublicKey(PublicKey.fromKeyStore(unshieldedKeystore));
      return unshielded;
    },
    dust: (config) =>
      DustWallet({
        ...config,
        // Fee headroom defaults; tx generation hits these when DUST is
        // tight on a fresh wallet.
        costParameters: {
          additionalFeeOverhead: DUST_FEE_OVERHEAD_SPECKS,
          feeBlocksMargin: 5,
        },
      }).startWithSecretKey(
        dustSecretKey,
        ledger.LedgerParameters.initialParameters().dust,
      ),
  });

  await wallet.start(shieldedSecretKeys, dustSecretKey);

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
  return await ctx.wallet.unshielded.getAddress();
}

/**
 * Throw unless the wallet holds enough DUST to pay a fee.
 *
 * NIGHT generates DUST at a fixed rate per unit held, up to a cap also
 * proportional to NIGHT, so an operator short of DUST either waits or funds
 * more NIGHT. The error reports both so they can tell which.
 */
export async function assertDustForFees(ctx: WalletContext): Promise<void> {
  const state = await waitForSync(ctx);
  const held = state.dust?.balance(new Date()) ?? 0n;
  const needed = minimumDustSpecks();
  if (held >= needed) return;

  const params = ledger.LedgerParameters.initialParameters().dust;
  const night = state.unshielded?.balances[ledger.nativeToken().raw] ?? 0n;
  const cap = night * params.nightDustRatio;
  const ratePerSec = night * params.generationDecayRate;
  const advice =
    cap < needed
      ? 'fund it with more NIGHT'
      : `retry in ~${(needed - held + ratePerSec - 1n) / ratePerSec}s`;
  throw new Error(
    `not enough DUST to pay fees: holds ${held} specks, needs ${needed}. ` +
      `${night} micro-NIGHT generates ${ratePerSec} specks/s up to ${cap} — ${advice}.`,
  );
}

/**
 * Register the wallet's unregistered NIGHT UTXOs for DUST generation.
 * Idempotent. Registration only starts generation, so this still throws when
 * the balance is short: DUST accrues over time, not on submission.
 */
export async function registerNightForDust(ctx: WalletContext): Promise<void> {
  const state = await waitForSync(ctx);

  const unregistered = (state.unshielded?.availableCoins ?? []).filter(
    (coin) => coin.meta?.registeredForDustGeneration === false,
  );

  if (unregistered.length > 0) {
    // The synced state's coins are a structural view; the facade wants its
    // own UtxoWithMeta. Same shape, distinct declaration.
    const recipe = await ctx.wallet.registerNightUtxosForDustGeneration(
      unregistered as unknown as Parameters<
        typeof ctx.wallet.registerNightUtxosForDustGeneration
      >[0],
      ctx.unshieldedKeystore.getPublicKey(),
      (payload: Uint8Array) => ctx.unshieldedKeystore.signDataAsync(payload),
    );
    const finalized = await ctx.wallet.finalizeRecipe(recipe);
    await ctx.wallet.submitTransaction(finalized);
  }

  await assertDustForFees(ctx);
}
