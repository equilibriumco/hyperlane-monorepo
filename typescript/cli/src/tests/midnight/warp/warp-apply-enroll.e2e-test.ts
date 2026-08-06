import { expect } from 'chai';
import fs from 'fs';
import path from 'path';

import {
  type CoreConfig,
  TokenStandard,
  TokenType,
  type WarpCoreConfig,
  type WarpRouteDeployConfig,
} from '@hyperlane-xyz/sdk';
import { ProtocolType } from '@hyperlane-xyz/utils';

import { readYamlOrJson, writeYamlOrJson } from '../../../utils/files.js';
import { HyperlaneE2ECoreTestCommands } from '../../commands/core.js';
import { HyperlaneE2EWarpTestCommands } from '../../commands/warp.js';
import {
  CHAIN_NAME_1,
  CORE_CONFIG_PATH,
  CORE_READ_CONFIG_PATH_1,
  HYP_KEY,
  MIDNIGHT_E2E_TEST_TIMEOUT,
  REGISTRY_PATH,
  REMOTE_CHAIN_NAME,
  REMOTE_DOMAIN_ID,
  REMOTE_HYP_KEY,
  REMOTE_ROUTER_ADDRESS,
  WARP_READ_OUTPUT_PATH_1,
} from '../consts.js';

const WARP_ROUTE_ID = `NIGHT/${CHAIN_NAME_1}`;
const WARP_ROUTE_DIR = `${REGISTRY_PATH}/deployments/warp_routes/NIGHT`;
const WARP_DEPLOY_PATH = `${WARP_ROUTE_DIR}/${CHAIN_NAME_1}-deploy.yaml`;
const WARP_CORE_PATH = `${WARP_ROUTE_DIR}/${CHAIN_NAME_1}-config.yaml`;

// Wire-format decimals are 18, NIGHT has 6: scale is sealed on-chain.
const NIGHT_SCALE = 1_000_000_000_000;

describe('hyperlane midnight warp apply e2e tests', async function () {
  this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

  const hyperlaneCore = new HyperlaneE2ECoreTestCommands(
    ProtocolType.Midnight,
    CHAIN_NAME_1,
    REGISTRY_PATH,
    CORE_CONFIG_PATH,
    CORE_READ_CONFIG_PATH_1,
  );

  const hyperlaneWarp = new HyperlaneE2EWarpTestCommands(
    ProtocolType.Midnight,
    REGISTRY_PATH,
    WARP_READ_OUTPUT_PATH_1,
  );

  let mailbox: string;
  let ownerCommitment: string;

  before(async function () {
    this.timeout(MIDNIGHT_E2E_TEST_TIMEOUT);

    // The night contract IS the native warp route; it is born in core
    // deploy, so a midnight warp route is registered, never deployed.
    const addresses = await hyperlaneCore.deployOrUseExistingCore(HYP_KEY);
    mailbox = addresses.mailbox;

    const coreConfig: CoreConfig = await hyperlaneCore.readConfig();
    ownerCommitment = coreConfig.owner;
  });

  function writeRouteConfigs(
    remoteRouters: Record<string, { address: string }>,
  ): void {
    // The remote chain entry is reference data only (foreignDeployment):
    // the CLI never submits there, but the signer middleware still wants a
    // key for it — REMOTE_HYP_KEY satisfies that.
    // Token metadata rides the config: non-EVM native entries are not
    // derived from chain metadata by the shared token-metadata code.
    const deployConfig: WarpRouteDeployConfig = {
      [CHAIN_NAME_1]: {
        type: TokenType.native,
        name: 'NIGHT',
        symbol: 'NIGHT',
        decimals: 6,
        owner: ownerCommitment,
        mailbox,
        scale: NIGHT_SCALE,
        remoteRouters,
      },
      [REMOTE_CHAIN_NAME]: {
        type: TokenType.synthetic,
        owner: '0x0000000000000000000000000000000000000001',
        foreignDeployment: '0xf39fd6e51aad88f6f4ce6ab8827279cfffb92266',
        gas: 200000,
      },
    } as WarpRouteDeployConfig;

    const coreConfig: WarpCoreConfig = {
      tokens: [
        {
          chainName: CHAIN_NAME_1,
          standard: TokenStandard.MidnightHypNative,
          decimals: 6,
          symbol: 'NIGHT',
          name: 'NIGHT',
          addressOrDenom: mailbox,
        },
      ],
    };

    fs.mkdirSync(path.dirname(WARP_DEPLOY_PATH), { recursive: true });
    writeYamlOrJson(WARP_DEPLOY_PATH, deployConfig);
    writeYamlOrJson(WARP_CORE_PATH, coreConfig);
  }

  async function applyRoute(): Promise<void> {
    await hyperlaneWarp.applyRaw({
      warpRouteId: WARP_ROUTE_ID,
      privateKey: HYP_KEY,
      skipConfirmationPrompts: true,
      extraArgs: ['--key.ethereum', REMOTE_HYP_KEY],
    });
  }

  async function readRemoteRouters(): Promise<Record<string, unknown>> {
    await hyperlaneWarp.readRaw({
      chain: CHAIN_NAME_1,
      warpAddress: mailbox,
      outputPath: WARP_READ_OUTPUT_PATH_1,
    });

    const readConfig: WarpRouteDeployConfig = readYamlOrJson(
      WARP_READ_OUTPUT_PATH_1,
    );
    return (readConfig[CHAIN_NAME_1].remoteRouters ?? {}) as Record<
      string,
      unknown
    >;
  }

  it('should enroll and unenroll a remote router', async () => {
    writeRouteConfigs({
      [REMOTE_DOMAIN_ID.toString()]: { address: REMOTE_ROUTER_ADDRESS },
    });
    await applyRoute();

    const enrolled = await readRemoteRouters();
    const enrolledRouter = enrolled[REMOTE_DOMAIN_ID.toString()] as
      | { address: string }
      | undefined;
    expect(enrolledRouter, 'expected the remote router to be enrolled').to.not
      .be.undefined;
    expect(enrolledRouter!.address.toLowerCase()).to.equal(
      REMOTE_ROUTER_ADDRESS,
    );

    writeRouteConfigs({});
    await applyRoute();

    const unenrolled = await readRemoteRouters();
    expect(unenrolled[REMOTE_DOMAIN_ID.toString()]).to.be.undefined;
  });
});
