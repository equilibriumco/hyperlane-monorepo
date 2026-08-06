import { expect } from 'chai';

import {
  type CoreConfig,
  IsmType,
  type MultisigIsmConfig,
} from '@hyperlane-xyz/sdk';
import { ProtocolType, assert } from '@hyperlane-xyz/utils';

import { readYamlOrJson, writeYamlOrJson } from '../../../utils/files.js';
import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
} from '../consts.js';

const ZERO_32_BYTES = `0x${'0'.repeat(64)}`;

describe('hyperlane midnight core apply e2e tests', async function () {
  this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

  const hyperlaneCore = new HyperlaneE2ECoreTestCommands(
    ProtocolType.Midnight,
    CHAIN_NAME_1,
    REGISTRY_PATH,
    CORE_CONFIG_PATH,
    CORE_READ_CONFIG_PATH_1,
  );

  let ownerCommitment: string;

  before(async function () {
    this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);
    await hyperlaneCore.deployOrUseExistingCore(HYP_KEY);

    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();
    ownerCommitment = coreConfig.owner;
  });

  // Static multisig ISMs are immutable on EVM, but the night contract
  // rotates validators in place via setValidatorsAndThreshold. The apply
  // config mirrors the deploy config except: owner pinned to the on-chain
  // commitment (avoids a spurious ownership transfer) and requiredHook as
  // a plain address reference (the IGP block is deploy-time input only —
  // repeating it here would deploy a second IGP).
  function applyConfig(threshold: number): CoreConfig {
    const inputConfig: CoreConfig = readYamlOrJson(CORE_CONFIG_PATH);
    const ism = inputConfig.defaultIsm as MultisigIsmConfig;

    return {
      ...inputConfig,
      owner: ownerCommitment,
      defaultIsm: { ...ism, threshold },
      requiredHook: ZERO_32_BYTES,
    };
  }

  async function applyAndReadThreshold(threshold: number): Promise<number> {
    writeYamlOrJson(CORE_READ_CONFIG_PATH_1, applyConfig(threshold));
    await hyperlaneCore.apply(HYP_KEY);

    const updatedConfig: CoreConfig = await hyperlaneCore.readConfig();
    const updatedIsm = updatedConfig.defaultIsm as MultisigIsmConfig;
    assert(
      updatedIsm.type === IsmType.MESSAGE_ID_MULTISIG,
      `Expected defaultIsm to be ${IsmType.MESSAGE_ID_MULTISIG}`,
    );
    return updatedIsm.threshold;
  }

  it('should rotate the multisig threshold in place and back', async () => {
    expect(await applyAndReadThreshold(1)).to.equal(1);
    expect(await applyAndReadThreshold(2)).to.equal(2);
  });
});
