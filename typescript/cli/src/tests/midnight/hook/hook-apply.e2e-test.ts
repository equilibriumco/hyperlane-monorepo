import { expect } from 'chai';

import { HookType, type IgpHookConfig } from '@hyperlane-xyz/sdk';
import { ProtocolType, assert } from '@hyperlane-xyz/utils';

import { writeYamlOrJson } from '../../../utils/files.js';
import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import { HyperlaneE2EHookTestCommands } from '../../commands/hook.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HOOK_APPLY_CONFIG_PATH_1,
  HOOK_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
  REMOTE_CHAIN_NAME,
} from '../consts.js';

describe('hyperlane midnight hook apply e2e tests', async function () {
  this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

  const hyperlaneCore = new HyperlaneE2ECoreTestCommands(
    ProtocolType.Midnight,
    CHAIN_NAME_1,
    REGISTRY_PATH,
    CORE_CONFIG_PATH,
    CORE_READ_CONFIG_PATH_1,
  );

  const hyperlaneHook = new HyperlaneE2EHookTestCommands(
    ProtocolType.Midnight,
    CHAIN_NAME_1,
    REGISTRY_PATH,
  );

  let igpAddress: string;

  before(async function () {
    this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

    // The IGP has no on-chain hook slot on the mailbox; core deploy created
    // it from the requiredHook config block and recorded it in the registry.
    const addresses = await hyperlaneCore.deployOrUseExistingCore(HYP_KEY);
    igpAddress = addresses.interchainGasPaymaster;
  });

  async function readGasPrice(): Promise<{
    config: IgpHookConfig;
    gasPrice: string;
  }> {
    const config = (await hyperlaneHook.readConfig(
      igpAddress,
      HOOK_READ_CONFIG_PATH_1,
    )) as IgpHookConfig;

    assert(
      config.type === HookType.INTERCHAIN_GAS_PAYMASTER,
      `Expected hook at ${igpAddress} to be an IGP`,
    );
    const oracle = config.oracleConfig[REMOTE_CHAIN_NAME];
    assert(oracle, `No gas oracle entry for ${REMOTE_CHAIN_NAME}`);
    return { config, gasPrice: oracle.gasPrice };
  }

  async function applyGasPrice(
    baseConfig: IgpHookConfig,
    gasPrice: string,
  ): Promise<void> {
    const applyConfig: IgpHookConfig = {
      ...baseConfig,
      oracleConfig: {
        ...baseConfig.oracleConfig,
        [REMOTE_CHAIN_NAME]: {
          ...baseConfig.oracleConfig[REMOTE_CHAIN_NAME],
          gasPrice,
        },
      },
    };
    writeYamlOrJson(HOOK_APPLY_CONFIG_PATH_1, applyConfig);
    await hyperlaneHook.apply(HYP_KEY, igpAddress, HOOK_APPLY_CONFIG_PATH_1);
  }

  it('should update the remote gas oracle and revert it', async () => {
    const initial = await readGasPrice();
    expect(initial.gasPrice).to.equal('1');

    await applyGasPrice(initial.config, '2');
    const updated = await readGasPrice();
    expect(updated.gasPrice).to.equal('2');

    await applyGasPrice(updated.config, '1');
    const reverted = await readGasPrice();
    expect(reverted.gasPrice).to.equal('1');
  });

  async function applyOverhead(
    baseConfig: IgpHookConfig,
    overhead: number,
  ): Promise<void> {
    const applyConfig: IgpHookConfig = {
      ...baseConfig,
      overhead: {
        ...baseConfig.overhead,
        [REMOTE_CHAIN_NAME]: overhead,
      },
    };
    writeYamlOrJson(HOOK_APPLY_CONFIG_PATH_1, applyConfig);
    await hyperlaneHook.apply(HYP_KEY, igpAddress, HOOK_APPLY_CONFIG_PATH_1);
  }

  it('should update the destination gas overhead and revert it', async () => {
    const initial = await readGasPrice();
    const initialOverhead = initial.config.overhead[REMOTE_CHAIN_NAME];
    expect(initialOverhead).to.equal(50000);

    await applyOverhead(initial.config, 60000);
    const updated = await readGasPrice();
    expect(updated.config.overhead[REMOTE_CHAIN_NAME]).to.equal(60000);
    // gasPrice rides along unchanged in the same oracle slot.
    expect(updated.gasPrice).to.equal(initial.gasPrice);

    await applyOverhead(updated.config, initialOverhead);
    const reverted = await readGasPrice();
    expect(reverted.config.overhead[REMOTE_CHAIN_NAME]).to.equal(
      initialOverhead,
    );
  });
});
