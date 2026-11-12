import type { RawIsmArtifactConfigs } from '@hyperlane-xyz/provider-sdk/ism';

// The well-known anvil dev accounts #0-#2: real secp256k1 pubkeys whose
// keccak hashes derive the matching addresses.
export const VALIDATORS = [
  '0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266',
  '0x70997970C51812dc3A010C7d01b50e0d17dc79C8',
  '0x3C44CdDdB6a900fa2b585dd299e03d12FA4293BC',
];
export const PUBKEYS = [
  '0x8318535b54105d4a7aae60c08fc45f9687181b4fdfc625bd1a753fa7397fed753547f11ca8696646f2f3acb08e31016afac23e630c5d11f59f61fef57b0d2aa5',
  '0xba5734d8f7091719471e7f7ed6b9df170dc70cc661ca05e688601ad984f068b0d67351e5f06073092499336ab0839ef8a521afd334e53807205fa2f08eec74f4',
  '0x9d9031e97dd78ff8c15aa86939de9b1e791066a0224e331bc962a2099a7b1f0464b8bbafe1535f2301c72c2cb3535b172da30b02686ab0393d348614f157fbdb',
];

export type MultisigConfig = RawIsmArtifactConfigs['messageIdMultisigIsm'];

// A multisig config over the accounts at `members`, in that order.
export function multisigOf(
  members: number[],
  threshold: number,
): MultisigConfig {
  return {
    type: 'messageIdMultisigIsm',
    validators: members.map((i) => VALIDATORS[i]),
    threshold,
    validatorPubkeys: members.map((i) => PUBKEYS[i]),
  };
}
