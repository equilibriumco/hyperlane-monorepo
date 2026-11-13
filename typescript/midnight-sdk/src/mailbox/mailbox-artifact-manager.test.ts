import { expect } from 'chai';

import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactOnChain,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedIsmAddress,
  IsmArtifactConfig,
  RawIsmArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/ism';
import type { MailboxOnChain } from '@hyperlane-xyz/provider-sdk/mailbox';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import {
  MidnightMultisigIsmWriter,
  MidnightRoutingIsmWriter,
} from '../ism/ism-artifact-manager.js';
import { validatorSetsByOrigin } from '../ism/validators.js';
import { rejectionOf } from '../testing/assertions.js';
import {
  NIGHT_DOMAIN,
  NIGHT_METADATA,
  SimulatedNight,
  deployNight,
} from '../testing/simulated-night.js';
import { multisigOf, type MultisigConfig } from '../testing/validators.js';
import { bytesToHex } from '../utils/conversion.js';
import type { MidnightTransaction } from '../utils/types.js';

import {
  MidnightMailboxWriter,
  type NightDeployer,
} from './mailbox-artifact-manager.js';

type IsmArtifact = ArtifactOnChain<IsmArtifactConfig, DeployedIsmAddress>;

const ANVIL2 = 31338;
const ANVIL3 = 31347;
const ANVIL4 = 31348;

const NO_HOOK = {
  artifactState: ArtifactState.UNDERIVED,
  deployed: { address: ZERO_ADDRESS_HEX_32 },
} as const;

// A signer whose deploy and transactions run on the simulated night.
function simulatedSigner(night: SimulatedNight) {
  const sent: MidnightTransaction[] = [];
  const deployed: { constructorArgs?: unknown[] } = {};
  const signer: NightDeployer = {
    async deployMidnightContract(options) {
      const ownerId = await night.ownerId();
      deployed.constructorArgs = options.buildArgs({
        ownerId,
        instanceSalt: night.instanceSalt,
        deployerUnshielded: new Uint8Array(32),
      });
      await night.deploy(deployed.constructorArgs);
      return {
        address: night.address,
        ownerId,
        receipts: [{ txId: 'deploy', blockHeight: 1 }],
      };
    },
    async sendAndConfirmTransaction(tx) {
      await night.call(tx.circuit, tx.args);
      sent.push(tx);
      return { txId: `tx-${sent.length}`, blockHeight: 1 + sent.length };
    },
  };
  return { signer, sent, deployed };
}

// The ISM as the generic core writer hands it to the mailbox writer on a
// deploy: every origin through the multisig writer's create, then the routing
// writer's create, all still at the zero-address sentinel.
async function newRoutingIsm(
  night: SimulatedNight,
  sets: Record<number, MultisigConfig>,
): Promise<
  ArtifactDeployed<
    RawIsmArtifactConfigs['domainRoutingIsm'],
    DeployedIsmAddress
  >
> {
  const domains: Record<number, IsmArtifact> = {};
  for (const [origin, config] of Object.entries(sets)) {
    [domains[Number(origin)]] = await new MidnightMultisigIsmWriter().create({
      config,
    });
  }
  const [routingIsm] = await new MidnightRoutingIsmWriter(night).create({
    config: { type: 'domainRoutingIsm', owner: ZERO_ADDRESS_HEX_32, domains },
  });
  return routingIsm;
}

function mailboxConfig(defaultIsm: IsmArtifact): MailboxOnChain {
  return {
    owner: ZERO_ADDRESS_HEX_32,
    defaultIsm,
    defaultHook: NO_HOOK,
    requiredHook: NO_HOOK,
  };
}

describe('Midnight mailbox writer', () => {
  describe('create', () => {
    it('deploys with the lowest origin and sets the others afterwards', async () => {
      const night = new SimulatedNight();
      const { signer, sent, deployed } = simulatedSigner(night);
      const writer = new MidnightMailboxWriter(night, NIGHT_METADATA, signer);
      const routingIsm = await newRoutingIsm(night, {
        [ANVIL4]: multisigOf([1, 0], 2),
        [ANVIL2]: multisigOf([0, 1, 2], 2),
        [ANVIL3]: multisigOf([2], 1),
      });

      const [mailbox, receipts] = await writer.create({
        config: mailboxConfig(routingIsm),
      });

      // localDomain, then the constructor's one origin.
      expect(deployed.constructorArgs?.slice(2, 4)).to.eql([
        BigInt(NIGHT_DOMAIN),
        BigInt(ANVIL2),
      ]);
      expect(sent.map((tx) => [tx.circuit, tx.args[0]])).to.eql([
        ['setValidatorsAndThreshold', BigInt(ANVIL3)],
        ['setValidatorsAndThreshold', BigInt(ANVIL4)],
      ]);
      expect(receipts.map((receipt) => receipt.txId)).to.eql([
        'deploy',
        'tx-1',
        'tx-2',
      ]);

      // Night's address is stamped onto the routing ISM and every origin's.
      expect(routingIsm.deployed.address).to.equal(night.address);
      expect(
        Object.values(routingIsm.config.domains).map(
          (origin) => origin.deployed.address,
        ),
      ).to.eql([night.address, night.address, night.address]);
      expect(mailbox.config.defaultIsm).to.equal(routingIsm);
      expect(mailbox.deployed).to.eql({
        address: night.address,
        domainId: NIGHT_DOMAIN,
      });
      expect(mailbox.config.owner).to.equal(bytesToHex(await night.ownerId()));

      // The chain holds every origin's set, and the orchestrator's update
      // step, which gets the same stamped ISM, has nothing left to do.
      const onChain = await new MidnightRoutingIsmWriter(night).read(
        night.address,
      );
      expect(
        [...validatorSetsByOrigin(onChain.config.domains)].map(
          ([origin, set]) => [origin, set.threshold, set.validatorPubkeys],
        ),
      ).to.eql([
        [ANVIL2, 2, multisigOf([0, 1, 2], 2).validatorPubkeys],
        [ANVIL3, 1, multisigOf([2], 1).validatorPubkeys],
        [ANVIL4, 2, multisigOf([1, 0], 2).validatorPubkeys],
      ]);
      expect(
        await writer.update({ ...mailbox, config: mailboxConfig(routingIsm) }),
      ).to.eql([]);
    });

    it('refuses an ISM that is not a routing ISM of multisigs', async () => {
      const night = new SimulatedNight();
      const { signer, deployed } = simulatedSigner(night);
      const writer = new MidnightMailboxWriter(night, NIGHT_METADATA, signer);
      const [multisigIsm] = await new MidnightMultisigIsmWriter().create({
        config: multisigOf([0, 1, 2], 2),
      });

      for (const defaultIsm of [
        multisigIsm,
        {
          artifactState: ArtifactState.UNDERIVED,
          deployed: { address: night.address },
        },
      ]) {
        expect(
          await rejectionOf(
            writer.create({ config: mailboxConfig(defaultIsm) }),
          ),
        ).to.include("write night's ISM as a domainRoutingIsm");
      }
      expect(deployed.constructorArgs).to.equal(undefined);
    });
  });

  describe('update', () => {
    let night: SimulatedNight;
    let writer: MidnightMailboxWriter;

    function mailboxWith(ismAddress: string) {
      return {
        artifactState: ArtifactState.DEPLOYED,
        config: mailboxConfig({
          artifactState: ArtifactState.UNDERIVED,
          deployed: { address: ismAddress },
        }),
        deployed: { address: night.address },
      };
    }

    beforeEach(async () => {
      night = await deployNight({ [ANVIL2]: multisigOf([0, 1, 2], 2) });
      writer = new MidnightMailboxWriter(
        night,
        NIGHT_METADATA,
        simulatedSigner(night).signer,
      );
    });

    it('takes night itself as the ISM', async () => {
      expect(await writer.update(mailboxWith(night.address))).to.eql([]);
    });

    // A config with the ISM as one top-level multisig makes the generic flow
    // "deploy a new ISM", which on Midnight is only the zero-address sentinel.
    it('refuses the sentinel a stale ISM shape ends up as', async () => {
      expect(
        await rejectionOf(writer.update(mailboxWith(ZERO_ADDRESS_HEX_32))),
      ).to.include("write night's ISM as a domainRoutingIsm");
    });

    it('refuses any other ISM address', async () => {
      const other = `0x${'77'.repeat(32)}`;
      expect(await rejectionOf(writer.update(mailboxWith(other)))).to.include(
        `cannot use ${other}`,
      );
    });

    it('reports the owner the routing ISM reports', async () => {
      const mailbox = await writer.read(night.address);
      const ism = await new MidnightRoutingIsmWriter(night).read(night.address);
      expect(ism.config.owner).to.equal(mailbox.config.owner);
    });
  });
});
