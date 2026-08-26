export function hexToBytes(hex: string): Uint8Array {
  const trimmed = hex.startsWith('0x') ? hex.slice(2) : hex;
  if (trimmed.length % 2 !== 0) {
    throw new Error(`invalid hex (odd length): ${hex}`);
  }
  const out = new Uint8Array(trimmed.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(trimmed.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

export function bytesToHex(bytes: Uint8Array): string {
  let out = '0x';
  for (const b of bytes) {
    out += b.toString(16).padStart(2, '0');
  }
  return out;
}
