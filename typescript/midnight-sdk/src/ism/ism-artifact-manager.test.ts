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
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { rejectionOf } from '../testing/assertions.js';
import {
  NIGHT_METADATA,
  deployNight,
  type SimulatedNight,
} from '../testing/simulated-night.js';
import {
  PUBKEYS,
  multisigOf,
  type MultisigConfig,
} from '../testing/validators.js';
import { bytesToHex, hexToBytes } from '../utils/conversion.js';
import type { MidnightTransaction } from '../utils/types.js';

import {
  MidnightIsmArtifactManager,
  MidnightMultisigIsmWriter,
  MidnightRoutingIsmWriter,
} from './ism-artifact-manager.js';
import { maxValidators } from './validators.js';

type RoutingIsmArtifact = ArtifactDeployed<
  RawIsmArtifactConfigs['domainRoutingIsm'],
  DeployedIsmAddress
>;
type OriginIsm = ArtifactOnChain<IsmArtifactConfig, DeployedIsmAddress>;

const ANVIL2 = 31338;
const ANVIL3 = 31347;
const ANVIL4 = 31348;

// The reader reports validators as the lowercase addresses their pubkeys
// derive.
function asRead(config: MultisigConfig): MultisigConfig {
  return {
    ...config,
    validators: config.validators.map((v) => v.toLowerCase()),
  };
}

// An origin as the generic routing writer hands it over: unchanged, so it
// still carries night's address.
function unchanged(night: SimulatedNight, config: MultisigConfig): OriginIsm {
  return {
    artifactState: ArtifactState.DEPLOYED,
    config,
    deployed: { address: night.address },
  };
}

// An origin whose config is new or changed: the generic routing writer passes
// it through the multisig writer's create first.
async function changed(config: MultisigConfig): Promise<OriginIsm> {
  const [created] = await new MidnightMultisigIsmWriter().create({ config });
  return created;
}

function routingIsm(
  night: SimulatedNight,
  domains: Record<number, OriginIsm>,
  owner: string = ZERO_ADDRESS_HEX_32,
): RoutingIsmArtifact {
  return {
    artifactState: ArtifactState.DEPLOYED,
    config: { type: 'domainRoutingIsm', owner, domains },
    deployed: { address: night.address },
  };
}

async function apply(
  night: SimulatedNight,
  txs: MidnightTransaction[],
): Promise<void> {
  for (const tx of txs) {
    await night.call(tx.circuit, tx.args);
  }
}

describe('Midnight routing ISM', () => {
  let night: SimulatedNight;
  let writer: MidnightRoutingIsmWriter;

  beforeEach(async () => {
    night = await deployNight({
      [ANVIL2]: multisigOf([0, 1, 2], 2),
      [ANVIL3]: multisigOf([2], 1),
    });
    writer = new MidnightRoutingIsmWriter(night);
  });

  describe('read', () => {
    it('reports one multisig per origin, each at the night address', async () => {
      const { data } = await night.requireContractState(night.address);
      const owner = await night.runCircuit<Uint8Array>('night', data, 'owner');

      expect(await writer.read(night.address)).to.eql({
        artifactState: ArtifactState.DEPLOYED,
        config: {
          type: 'domainRoutingIsm',
          owner: bytesToHex(owner),
          domains: {
            [ANVIL2]: unchanged(night, asRead(multisigOf([0, 1, 2], 2))),
            [ANVIL3]: unchanged(night, asRead(multisigOf([2], 1))),
          },
        },
        deployed: { address: night.address },
      });
    });

    it('has no multisig ISM to read on its own', async () => {
      const manager = new MidnightIsmArtifactManager(NIGHT_METADATA);
      expect(() => manager.createReader('messageIdMultisigIsm')).to.throw(
        'read its ISM as a domainRoutingIsm',
      );
      expect(
        await rejectionOf(new MidnightMultisigIsmWriter().read()),
      ).to.include('read its ISM as a domainRoutingIsm');
    });
  });

  describe('create', () => {
    it('validates an origin and hands back the zero-address sentinel', async () => {
      const config = multisigOf([0, 1], 2);
      const [created, receipts] = await new MidnightMultisigIsmWriter().create({
        config,
      });
      expect(created).to.eql({
        artifactState: ArtifactState.DEPLOYED,
        config,
        deployed: { address: ZERO_ADDRESS_HEX_32 },
      });
      expect(receipts).to.eql([]);

      expect(
        await rejectionOf(
          new MidnightMultisigIsmWriter().create({
            config: { ...config, validatorPubkeys: undefined },
          }),
        ),
      ).to.include('validatorPubkeys');
    });

    it('hands back the zero-address sentinel with every origin', async () => {
      const config: RawIsmArtifactConfigs['domainRoutingIsm'] = {
        type: 'domainRoutingIsm',
        owner: ZERO_ADDRESS_HEX_32,
        domains: {
          [ANVIL2]: await changed(multisigOf([0, 1, 2], 2)),
          [ANVIL3]: await changed(multisigOf([2], 1)),
        },
      };
      const [created, receipts] = await writer.create({ config });
      expect(created).to.eql({
        artifactState: ArtifactState.DEPLOYED,
        config,
        deployed: { address: ZERO_ADDRESS_HEX_32 },
      });
      expect(receipts).to.eql([]);
    });

    it('needs at least one origin', async () => {
      expect(
        await rejectionOf(
          writer.create({
            config: {
              type: 'domainRoutingIsm',
              owner: ZERO_ADDRESS_HEX_32,
              domains: {},
            },
          }),
        ),
      ).to.include('at least one origin chain');
    });

    it('refuses an origin given as an ISM address', async () => {
      expect(
        await rejectionOf(
          writer.create({
            config: {
              type: 'domainRoutingIsm',
              owner: ZERO_ADDRESS_HEX_32,
              domains: {
                [ANVIL2]: {
                  artifactState: ArtifactState.UNDERIVED,
                  deployed: { address: night.address },
                },
              },
            },
          }),
        ),
      ).to.include(`origin ${ANVIL2}: write night's ISM`);
    });
  });

  describe('update', () => {
    it('emits nothing when every set is already on chain', async () => {
      const txs = await writer.update(
        routingIsm(night, {
          [ANVIL2]: unchanged(night, multisigOf([0, 1, 2], 2)),
          [ANVIL3]: unchanged(night, multisigOf([2], 1)),
        }),
      );
      expect(txs).to.eql([]);
    });

    it('never updates through the multisig writer', async () => {
      const txs = await new MidnightMultisigIsmWriter().update();
      expect(txs).to.eql([]);
    });

    it('replaces only the origin whose set changed', async () => {
      const txs = await writer.update(
        routingIsm(night, {
          [ANVIL2]: await changed(multisigOf([0, 1], 1)),
          [ANVIL3]: unchanged(night, multisigOf([2], 1)),
        }),
      );

      const slots = await maxValidators();
      expect(txs).to.have.lengthOf(1);
      expect(txs[0]).to.include({
        contract: 'night',
        contractAddress: night.address,
        circuit: 'setValidatorsAndThreshold',
      });
      expect(txs[0].args).to.eql([
        BigInt(ANVIL2),
        [
          hexToBytes(PUBKEYS[0]),
          hexToBytes(PUBKEYS[1]),
          ...Array.from({ length: slots - 2 }, () => new Uint8Array(64)),
        ],
        2n,
        1n,
      ]);

      await apply(night, txs);
      expect((await writer.read(night.address)).config.domains).to.eql({
        [ANVIL2]: unchanged(night, asRead(multisigOf([0, 1], 1))),
        [ANVIL3]: unchanged(night, asRead(multisigOf([2], 1))),
      });
    });

    it('sets a new origin', async () => {
      const txs = await writer.update(
        routingIsm(night, {
          [ANVIL2]: unchanged(night, multisigOf([0, 1, 2], 2)),
          [ANVIL3]: unchanged(night, multisigOf([2], 1)),
          [ANVIL4]: await changed(multisigOf([1, 0], 2)),
        }),
      );

      expect(txs.map((tx) => [tx.circuit, tx.args[0]])).to.eql([
        ['setValidatorsAndThreshold', BigInt(ANVIL4)],
      ]);

      await apply(night, txs);
      expect((await writer.read(night.address)).config.domains).to.eql({
        [ANVIL2]: unchanged(night, asRead(multisigOf([0, 1, 2], 2))),
        [ANVIL3]: unchanged(night, asRead(multisigOf([2], 1))),
        [ANVIL4]: unchanged(night, asRead(multisigOf([1, 0], 2))),
      });
    });

    it('removes an origin that is no longer in the config', async () => {
      const txs = await writer.update(
        routingIsm(night, {
          [ANVIL2]: unchanged(night, multisigOf([0, 1, 2], 2)),
        }),
      );

      expect(txs).to.have.lengthOf(1);
      expect(txs[0]).to.include({
        contract: 'night',
        contractAddress: night.address,
        circuit: 'removeValidators',
      });
      expect(txs[0].args).to.eql([BigInt(ANVIL3)]);

      await apply(night, txs);
      expect((await writer.read(night.address)).config.domains).to.eql({
        [ANVIL2]: unchanged(night, asRead(multisigOf([0, 1, 2], 2))),
      });
    });

    it('refuses a config that would leave no origin', async () => {
      expect(
        await rejectionOf(writer.update(routingIsm(night, {}))),
      ).to.include('at least one origin chain');
    });

    it('takes the zero owner or night owner, and refuses any other', async () => {
      const domains = {
        [ANVIL2]: unchanged(night, multisigOf([0, 1, 2], 2)),
        [ANVIL3]: unchanged(night, multisigOf([2], 1)),
      };
      const { owner } = (await writer.read(night.address)).config;

      expect(await writer.update(routingIsm(night, domains, owner))).to.eql([]);
      // The same commitment without the 0x prefix is the same owner.
      expect(
        await writer.update(routingIsm(night, domains, owner.slice(2))),
      ).to.eql([]);

      const other = `0x${'77'.repeat(32)}`;
      expect(
        await rejectionOf(writer.update(routingIsm(night, domains, other))),
      ).to.include(`the config says ${other}`);
    });

    it('refuses an origin given as an ISM address', async () => {
      expect(
        await rejectionOf(
          writer.update(
            routingIsm(night, {
              [ANVIL2]: {
                artifactState: ArtifactState.UNDERIVED,
                deployed: { address: night.address },
              },
            }),
          ),
        ),
      ).to.include(`origin ${ANVIL2}: write night's ISM`);
    });
  });
});
