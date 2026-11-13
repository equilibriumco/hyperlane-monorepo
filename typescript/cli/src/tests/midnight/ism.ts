import {
  type CoreConfig,
  IsmType,
  type MultisigIsmConfig,
} from '@hyperlane-xyz/sdk';
import { assert } from '@hyperlane-xyz/utils';

// Night's ISM in a core config: one message-id multisig per origin chain.
export function validatorSetsByOrigin(
  config: CoreConfig,
): Record<string, MultisigIsmConfig> {
  const ism = config.defaultIsm;
  assert(
    typeof ism !== 'string' && ism.type === IsmType.ROUTING,
    `Expected defaultIsm to be ${IsmType.ROUTING}`,
  );
  const sets: Record<string, MultisigIsmConfig> = {};
  for (const [origin, originIsm] of Object.entries(ism.domains)) {
    assert(
      typeof originIsm !== 'string' &&
        originIsm.type === IsmType.MESSAGE_ID_MULTISIG,
      `Expected the ISM for ${origin} to be ${IsmType.MESSAGE_ID_MULTISIG}`,
    );
    sets[origin] = originIsm;
  }
  return sets;
}

// A set as `core read` reports it next to the same set as a config writes it:
// the reader lowercases the addresses its pubkeys derive.
export function comparableSet(set: MultisigIsmConfig) {
  return {
    threshold: set.threshold,
    validators: set.validators.map((validator) => validator.toLowerCase()),
    validatorPubkeys: set.validatorPubkeys?.map((pubkey) =>
      pubkey.toLowerCase(),
    ),
  };
}
