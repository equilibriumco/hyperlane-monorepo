export enum ProtocolType {
  Ethereum = 'ethereum',
  Sealevel = 'sealevel',
  Cosmos = 'cosmos',
  CosmosNative = 'cosmosnative',
  Starknet = 'starknet',
  Radix = 'radix',
  Aleo = 'aleo',
  Tron = 'tron',
  Cardano = 'cardano',
  Unknown = 'unknown',
}

// A type that also allows for literal values of the enum
export type ProtocolTypeValue = `${ProtocolType}`;

export const ProtocolSmallestUnit = {
  [ProtocolType.Ethereum]: 'wei',
  [ProtocolType.Sealevel]: 'lamports',
  [ProtocolType.Cosmos]: 'uATOM',
  [ProtocolType.CosmosNative]: 'uATOM',
  [ProtocolType.Starknet]: 'fri',
  [ProtocolType.Radix]: 'attos',
  [ProtocolType.Aleo]: 'microcredits',
  [ProtocolType.Tron]: 'SUN',
  [ProtocolType.Cardano]: 'lovelace',
  [ProtocolType.Unknown]: 'unknown',
};
