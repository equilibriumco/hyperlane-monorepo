import { expect } from 'chai';

import { type ChainAddresses } from '@hyperlane-xyz/registry';
import { type CoreConfig } from '@hyperlane-xyz/sdk';
import { ProtocolType } from '@hyperlane-xyz/utils';

import { readYamlOrJson } from '../../../utils/files.js';
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
} from '../consts.js';
import { comparableSet, validatorSetsByOrigin } from '../ism.js';

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

  // The constructor takes one origin's validators, so a deploy with two
  // origins is the deploy plus one owner transaction for the second.
  it('should deploy with a validator set for each of two origin chains', async () => {
    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();

    const expectedSets = validatorSetsByOrigin(inputConfig);
    const actualSets = validatorSetsByOrigin(coreConfig);

    expect(Object.keys(expectedSets).sort()).to.eql([
      REMOTE_CHAIN_NAME,
      REMOTE_CHAIN_NAME_2,
    ]);
    // `core read` shows one entry per origin, and the reader emits the
    // enrolled pubkeys so read output round-trips into write-ready configs.
    expect(Object.keys(actualSets).sort()).to.eql(
      Object.keys(expectedSets).sort(),
    );
    for (const [origin, expectedSet] of Object.entries(expectedSets)) {
      expect(comparableSet(actualSets[origin])).to.eql(
        comparableSet(expectedSet),
      );
    }
  });

  it('should report night itself as the ISM of every origin', async () => {
    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();

    // Read back as written, the ISM carries the address it was read from.
    const ism: unknown = coreConfig.defaultIsm;
    expect(ism).to.have.property('address', addresses.mailbox);
    expect(ism).to.have.property('owner', coreConfig.owner);
    for (const origin of [REMOTE_CHAIN_NAME, REMOTE_CHAIN_NAME_2]) {
      expect(ism).to.have.nested.property(
        `domains.${origin}.address`,
        addresses.mailbox,
      );
    }
  });

  it('should keep ownership with the deployer for a zero owner config', async () => {
    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();

    // The on-chain owner is a ZOwnablePK commitment: a non-zero 32-byte
    // value that cannot equal any configured address.
    expect(coreConfig.owner).to.match(HEX_32_BYTES);
    expect(coreConfig.owner).to.not.equal(ZERO_32_BYTES);
  });
});
