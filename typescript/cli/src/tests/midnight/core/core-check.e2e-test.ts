import { expect } from 'chai';

import { ProtocolType } from '@hyperlane-xyz/utils';

import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
} from '../consts.js';

describe('hyperlane midnight core check e2e tests', async function () {
  this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

  const hyperlaneCore = new HyperlaneE2ECoreTestCommands(
    ProtocolType.Midnight,
    CHAIN_NAME_1,
    REGISTRY_PATH,
    CORE_CONFIG_PATH,
    CORE_READ_CONFIG_PATH_1,
  );

  before(async function () {
    this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);
    await hyperlaneCore.deployOrUseExistingCore(HYP_KEY);
  });

  it('should find no diff between the read output and the chain', async () => {
    // Round-trip: the read output (owner commitment, requiredHook as a
    // zero address reference) must check clean against the same chain.
    await hyperlaneCore.readConfig();

    const output = await hyperlaneCore.check();
    expect(output.exitCode).to.equal(0);
  });
});
