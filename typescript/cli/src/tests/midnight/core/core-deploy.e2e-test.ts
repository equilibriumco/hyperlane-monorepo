import { expect } from 'chai';

import { type ChainAddresses } from '@hyperlane-xyz/registry';
import {
  type CoreConfig,
  IsmType,
  type MultisigIsmConfig,
} from '@hyperlane-xyz/sdk';
import { ProtocolType, assert } from '@hyperlane-xyz/utils';

import { readYamlOrJson } from '../../../utils/files.js';
import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
} from '../consts.js';

const HEX_32_BYTES = /^0x[0-9a-f]{64}$/;
const ZERO_32_BYTES = `0x${'0'.repeat(64)}`;

describe('hyperlane midnight core deploy e2e tests', async function () {
  this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

  const hyperlaneCore = new HyperlaneE2ECoreTestCommands(
    ProtocolType.Midnight,
    CHAIN_NAME_1,
    REGISTRY_PATH,
    CORE_CONFIG_PATH,
    CORE_READ_CONFIG_PATH_1,
  );

  let inputConfig: CoreConfig;
  let addresses: ChainAddresses;

  before(async function () {
    this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

    inputConfig = readYamlOrJson(CORE_CONFIG_PATH);
    // The setup hooks clean the chain addresses once per run, so whichever
    // test file executes first pays for the single chunked deploy and the
    // rest reuse it.
    addresses = await hyperlaneCore.deployOrUseExistingCore(HYP_KEY);
  });

  it('should deploy the night monolith with its satellite contracts', () => {
    expect(addresses.mailbox).to.match(HEX_32_BYTES);
    expect(addresses.interchainGasPaymaster).to.match(HEX_32_BYTES);
    expect(addresses.validatorAnnounce).to.match(HEX_32_BYTES);

    // The night contract is its own merkle tree hook identity; the IGP and
    // the validator announce are standalone contracts.
    expect(addresses.merkleTreeHook).to.equal(addresses.mailbox);
    expect(addresses.interchainGasPaymaster).to.not.equal(addresses.mailbox);
    expect(addresses.validatorAnnounce).to.not.equal(addresses.mailbox);
    expect(addresses.validatorAnnounce).to.not.equal(
      addresses.interchainGasPaymaster,
    );
  });

  it('should seal the multisig ISM config into the deployed contract', async () => {
    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();

    const expectedIsm = inputConfig.defaultIsm as MultisigIsmConfig;
    const actualIsm = coreConfig.defaultIsm as MultisigIsmConfig & {
      validatorPubkeys?: string[];
    };

    assert(
      actualIsm.type === IsmType.MESSAGE_ID_MULTISIG,
      `Expected defaultIsm to be ${IsmType.MESSAGE_ID_MULTISIG}`,
    );
    expect(actualIsm.threshold).to.equal(expectedIsm.threshold);
    expect(actualIsm.validators.map((v) => v.toLowerCase()).sort()).to.eql(
      expectedIsm.validators.map((v) => v.toLowerCase()).sort(),
    );
    // The reader emits the enrolled pubkeys so read output round-trips
    // into write-ready configs.
    expect(actualIsm.validatorPubkeys).to.have.lengthOf(
      expectedIsm.validators.length,
    );
  });

  it('should keep ownership with the deployer for a zero owner config', async () => {
    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();

    // The on-chain owner is a ZOwnablePK commitment: a non-zero 32-byte
    // value that cannot equal any configured address.
    expect(coreConfig.owner).to.match(HEX_32_BYTES);
    expect(coreConfig.owner).to.not.equal(ZERO_32_BYTES);
  });
});
