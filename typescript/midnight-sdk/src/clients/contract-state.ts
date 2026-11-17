import { ContractState } from '@midnight-ntwrk/compact-runtime';
import type { ContractStateRead } from '@midnight-ntwrk/midnight-js-types';

/**
 * A state read decoded with this package's runtime. midnight-js hands state
 * over as bytes, since a contract's circuits take only objects of the runtime
 * copy the contract was compiled against.
 */
export function decodeContractState(
  read: Pick<ContractStateRead, 'contractState'>,
): ContractState {
  return read.contractState.deserialize({ v9: ContractState });
}
