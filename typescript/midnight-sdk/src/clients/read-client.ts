import { getPublicStates } from '@midnight-ntwrk/midnight-js-contracts';
import { indexerPublicDataProvider } from '@midnight-ntwrk/midnight-js-indexer-public-data-provider';

import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import { assert, strip0x } from '@hyperlane-xyz/utils';

import type { MidnightEndpoints } from '../utils/types.js';

import {
  loadContractModule,
  type MidnightContractName,
  runReadCircuit,
  witnessesFor,
} from './contracts.js';

const CONTRACT_NOT_FOUND_RE =
  /not found|no contract|unknown contract|no public state/i;

export type FetchedContractState = { data: unknown; balance: unknown };

// rpcUrls carry the node endpoint; gatewayUrls carry the indexer GraphQL
// endpoint, the read path for all chain state.
export function resolveEndpoints(
  metadata: ChainMetadataForAltVM,
): MidnightEndpoints {
  const [nodeUrl] = metadata.rpcUrls?.map(({ http }) => http) ?? [];
  assert(nodeUrl, `no rpcUrls in chain metadata for ${metadata.name}`);
  const [indexerGraphqlUrl] =
    metadata.gatewayUrls?.map(({ http }) => http) ?? [];
  assert(
    indexerGraphqlUrl,
    `no gatewayUrls (indexer GraphQL endpoint) in chain metadata for ${metadata.name}`,
  );
  const indexerWsUrl = `${indexerGraphqlUrl.replace(/^http/, 'ws')}/ws`;
  return { nodeUrl, indexerGraphqlUrl, indexerWsUrl };
}

export class MidnightReadClient {
  constructor(
    readonly chainName: string,
    readonly endpoints: MidnightEndpoints,
  ) {}

  static fromMetadata(metadata: ChainMetadataForAltVM): MidnightReadClient {
    return new MidnightReadClient(metadata.name, resolveEndpoints(metadata));
  }

  async fetchContractState(
    contractAddress: string,
  ): Promise<FetchedContractState | null> {
    const publicDataProvider = indexerPublicDataProvider(
      this.endpoints.indexerGraphqlUrl,
      this.endpoints.indexerWsUrl,
    );
    try {
      const publicStates = await getPublicStates(
        publicDataProvider,
        strip0x(contractAddress),
      );
      return publicStates.contractState;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      if (CONTRACT_NOT_FOUND_RE.test(message)) {
        return null;
      }
      throw err;
    }
  }

  async requireContractState(
    contractAddress: string,
  ): Promise<FetchedContractState> {
    const state = await this.fetchContractState(contractAddress);
    if (!state) {
      throw new Error(
        `Midnight contract ${contractAddress} not found on ${this.chainName}`,
      );
    }
    return state;
  }

  async runCircuit<T>(
    contract: MidnightContractName,
    stateData: unknown,
    circuitId: string,
    args: unknown[] = [],
  ): Promise<T> {
    const module = await loadContractModule(contract);
    return runReadCircuit<T>(
      module,
      witnessesFor(contract),
      stateData,
      circuitId,
      args,
    );
  }
}
