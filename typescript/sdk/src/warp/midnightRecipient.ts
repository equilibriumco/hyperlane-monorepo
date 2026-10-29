import {
  Address,
  addressToBytes32,
  decodeMidnightUnshieldedAddress,
  isZeroishAddress,
} from '@hyperlane-xyz/utils';

import type { ChainMetadata } from '../metadata/chainMetadataTypes.js';

// Recipient rules for Midnight routes. night pays an inbound transfer to the
// 32 recipient bytes as a user address, and the EVM router mints an outbound
// one to the last 20 bytes, so a recipient of the wrong kind is NIGHT nobody
// receives and night has no refund.

// A recipient on Midnight is taken in the wallet's unshielded address form,
// which names a user address and nothing else: a contract address or a
// shielded coin key cannot be told from a user address once they are hex.
export function validateMidnightRecipient(
  recipient: Address,
  destination: Pick<ChainMetadata, 'name' | 'midnightNetworkId'>,
): Record<string, string> | null {
  if (!recipient || !recipient.toLowerCase().startsWith('mn_')) {
    return {
      recipient:
        'Midnight recipients are given in the unshielded address form (mn_addr...), not as hex',
    };
  }
  let decoded: ReturnType<typeof decodeMidnightUnshieldedAddress>;
  try {
    decoded = decodeMidnightUnshieldedAddress(recipient);
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error);
    return { recipient: `Invalid Midnight address: ${reason}` };
  }
  const expected = destination.midnightNetworkId;
  if (expected && decoded.networkId !== expected) {
    return {
      recipient: `Recipient address is for the ${decoded.networkId} network, ${destination.name} is ${expected}`,
    };
  }
  if (decoded.bytes.every((b) => b === 0)) {
    return { recipient: 'Invalid recipient' };
  }
  return null;
}

// The 32 recipient bytes an EVM destination can mint to: a 20-byte address in
// the low bytes and nothing else. Its router reverts on anything else.
export function evmMintableRecipientBytes32(recipient: Address): string {
  const bytes32 = addressToBytes32(recipient);
  if (isZeroishAddress(bytes32)) {
    throw new Error(
      'recipient must not be the zero address: the destination cannot mint to it',
    );
  }
  if (!/^0x0{24}[0-9a-fA-F]{40}$/.test(bytes32)) {
    throw new Error(
      `recipient ${recipient} is not an EVM address: the first 12 of its 32 bytes must be zero, or the destination cannot mint to it`,
    );
  }
  return bytes32;
}
