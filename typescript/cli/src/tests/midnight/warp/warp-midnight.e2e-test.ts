import { expect } from 'chai';
import { $ } from 'zx';

import { ProtocolType } from '@hyperlane-xyz/utils';

import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import { localTestRunCmdPrefix } from '../../commands/helpers.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
} from '../consts.js';

// 2 NIGHT, from the deployer's wallet.
const FUND_AMOUNT = 2_000_000n;

function midnightCommand(command: string, extraArgs: string[] = []) {
  return $`${localTestRunCmdPrefix()} hyperlane warp midnight ${command} \
        --registry ${REGISTRY_PATH} \
        --chain ${CHAIN_NAME_1} \
        ${extraArgs}`;
}

function ownerCommand(command: string, extraArgs: string[] = []) {
  return midnightCommand(command, ['--key.midnight', HYP_KEY, ...extraArgs]);
}

async function readStatus(): Promise<{ paused: boolean; escrow: bigint }> {
  const { stdout } = await midnightCommand('status');
  const paused = stdout.match(/paused: (yes|no)/)?.[1];
  const escrow = stdout.match(/escrow: (\d+) micro-NIGHT/)?.[1];
  expect(paused, stdout).to.not.be.undefined;
  expect(escrow, stdout).to.not.be.undefined;
  return { paused: paused === 'yes', escrow: BigInt(escrow!) };
}

describe('hyperlane warp midnight e2e tests', async function () {
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

  it('should move NIGHT into the escrow', async () => {
    const before = await readStatus();
    await ownerCommand('fund', ['--amount', FUND_AMOUNT.toString()]);
    const after = await readStatus();
    expect(after.escrow - before.escrow).to.equal(FUND_AMOUNT);
  });

  it('should pause night until it is unpaused', async () => {
    expect((await readStatus()).paused).to.be.false;

    await ownerCommand('pause');
    expect((await readStatus()).paused).to.be.true;

    // night itself refuses an owner call while paused, so the pause is on
    // chain and not only in what the status read reports.
    const refused = await ownerCommand('fund', ['--amount', '1']).nothrow();
    expect(refused.exitCode).to.not.equal(0);
    expect(refused.stdout + refused.stderr).to.include('Pausable: paused');

    await ownerCommand('unpause');
    expect((await readStatus()).paused).to.be.false;
  });
});
