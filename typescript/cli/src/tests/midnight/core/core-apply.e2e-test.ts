import { expect } from 'chai';

import {
  type CoreConfig,
  IsmType,
  type MultisigIsmConfig,
} from '@hyperlane-xyz/sdk';
import { ProtocolType } from '@hyperlane-xyz/utils';

import { readYamlOrJson, writeYamlOrJson } from '../../../utils/files.js';
import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
  REMOTE_CHAIN_NAME,
  REMOTE_CHAIN_NAME_2,
  REMOTE_CHAIN_NAME_3,
} from '../consts.js';
import { comparableSet, validatorSetsByOrigin } from '../ism.js';

const ZERO_32_BYTES = `0x${'0'.repeat(64)}`;

type ValidatorSets = Record<string, MultisigIsmConfig>;

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
  // The sets the shared deploy starts with. Every test here leaves them as
  // they were, since the other test files read the same deployment.
  let deployedSets: ValidatorSets;

  before(async function () {
    this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);
    await hyperlaneCore.deployOrUseExistingCore(HYP_KEY);

    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();
    ownerCommitment = coreConfig.owner;
    deployedSets = validatorSetsByOrigin(readYamlOrJson(CORE_CONFIG_PATH));
  });

  // Mirrors the deploy config with three changes: the validator sets are the
  // ones given, the owner is pinned to the on-chain commitment, which avoids a
  // spurious ownership transfer, and requiredHook is a plain address, since
  // repeating the IGP block here would deploy a second IGP.
  function applyConfig(defaultIsm: CoreConfig['defaultIsm']): CoreConfig {
    const inputConfig: CoreConfig = readYamlOrJson(CORE_CONFIG_PATH);

    return {
      ...inputConfig,
      owner: ownerCommitment,
      defaultIsm,
      requiredHook: ZERO_32_BYTES,
    };
  }

  async function applyAndReadSets(sets: ValidatorSets): Promise<ValidatorSets> {
    writeYamlOrJson(
      CORE_READ_CONFIG_PATH_1,
      applyConfig({
        type: IsmType.ROUTING,
        owner: ZERO_32_BYTES,
        domains: sets,
      }),
    );
    await hyperlaneCore.apply(HYP_KEY);

    return validatorSetsByOrigin(await hyperlaneCore.readConfig());
  }

  function expectSets(actual: ValidatorSets, expected: ValidatorSets): void {
    expect(Object.keys(actual).sort()).to.eql(Object.keys(expected).sort());
    for (const [origin, expectedSet] of Object.entries(expected)) {
      expect(comparableSet(actual[origin])).to.eql(comparableSet(expectedSet));
    }
  }

  it("should change one origin's validator set in place and back", async () => {
    const rotated: ValidatorSets = {
      ...deployedSets,
      [REMOTE_CHAIN_NAME]: { ...deployedSets[REMOTE_CHAIN_NAME], threshold: 1 },
    };

    // The other origin's set is read back untouched both times.
    expectSets(await applyAndReadSets(rotated), rotated);
    expectSets(await applyAndReadSets(deployedSets), deployedSets);
  });

  it('should add an origin chain and remove it again', async () => {
    const added: ValidatorSets = {
      ...deployedSets,
      [REMOTE_CHAIN_NAME_3]: deployedSets[REMOTE_CHAIN_NAME_2],
    };

    expectSets(await applyAndReadSets(added), added);
    // An origin left out of the config is removed from the contract.
    expectSets(await applyAndReadSets(deployedSets), deployedSets);
  });

  // The ISM as it was written before night held a set per origin. It must
  // fail instead of applying nothing.
  it('should refuse the ISM written as a single multisig', async () => {
    writeYamlOrJson(
      CORE_READ_CONFIG_PATH_1,
      applyConfig(deployedSets[REMOTE_CHAIN_NAME]),
    );

    const output = await hyperlaneCore.apply(HYP_KEY).nothrow();

    expect(output.exitCode).to.not.equal(0);
    expect(output.text()).to.include("write night's ISM as a domainRoutingIsm");
    expectSets(
      validatorSetsByOrigin(await hyperlaneCore.readConfig()),
      deployedSets,
    );
  });
});
