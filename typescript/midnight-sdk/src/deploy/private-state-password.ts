// The local devnet's network id; its stores hold only throwaway secrets.
const DEVNET_NETWORK_ID = 'undeployed';
const DEVNET_PASSWORD = 'Hyperlane-Midnight-2026!';

let devnetPasswordNoted = false;

/**
 * The private-state store's password, asked for when the store is opened.
 * Off a local devnet the store holds real owner secrets, so a password written
 * in the source will not do there.
 */
export function privateStatePassword(networkId: string): string {
  const password = process.env.MIDNIGHT_STATE_PASSWORD;
  if (password) return password;
  if (networkId !== DEVNET_NETWORK_ID) {
    throw new Error(
      `MIDNIGHT_STATE_PASSWORD is required on ${networkId}: it encrypts the ` +
        'private-state store, which holds the owner secret',
    );
  }
  if (!devnetPasswordNoted) {
    devnetPasswordNoted = true;
    // eslint-disable-next-line no-console
    console.warn(
      'MIDNIGHT_STATE_PASSWORD unset: the private-state store uses the local devnet password',
    );
  }
  return DEVNET_PASSWORD;
}
