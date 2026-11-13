import type { ChainMetadataForAltVM } from '@hyperlane-xyz/provider-sdk';
import type { ISigner } from '@hyperlane-xyz/provider-sdk/altvm';
import {
  ArtifactState,
  type ArtifactDeployed,
  type ArtifactNew,
  type ArtifactReader,
  type ArtifactWriter,
} from '@hyperlane-xyz/provider-sdk/artifact';
import type {
  DeployedIsmAddress,
  DeployedRawIsmArtifact,
  IRawIsmArtifactManager,
  IsmType,
  RawIsmArtifactConfigs,
} from '@hyperlane-xyz/provider-sdk/ism';
import type {
  AnnotatedTx,
  TxReceipt,
} from '@hyperlane-xyz/provider-sdk/module';
import { ZERO_ADDRESS_HEX_32 } from '@hyperlane-xyz/utils';

import { bytesToHex, normalizeHexForCompare } from '../utils/conversion.js';
import { NIGHT_ISM_SHAPE, unsupportedOnMidnight } from '../utils/errors.js';
import type { MidnightTransaction } from '../utils/types.js';
import { MidnightReadClient } from '../clients/read-client.js';
import { readValidatorSetOrigins } from '../clients/state.js';

import {
  maxValidators,
  pubkeyToEthAddress,
  resolveValidatorSet,
  sameValidatorSet,
  setValidatorsTx,
  validatorSetsByOrigin,
} from './validators.js';

const MODULE_TYPE_MESSAGE_ID_MULTISIG = 5n;

type MultisigIsmConfig = RawIsmArtifactConfigs['messageIdMultisigIsm'];
type RoutingIsmConfig = RawIsmArtifactConfigs['domainRoutingIsm'];

// What night's validatorSet circuit returns: every slot, then how many of
// them are the set.
type OnChainValidatorSet = {
  validators: Uint8Array[];
  count: bigint;
  threshold: bigint;
};

function noSingleMultisigIsm(): never {
  throw new Error(
    'night keeps one validator set per origin chain, so there is no single ' +
      'messageIdMultisigIsm to read — read its ISM as a domainRoutingIsm',
  );
}

// One origin's validators are part of night, with no address or state of
// their own: the routing writer below reads and writes every origin's set.
export class MidnightMultisigIsmWriter implements ArtifactWriter<
  MultisigIsmConfig,
  DeployedIsmAddress
> {
  async read(): Promise<
    ArtifactDeployed<MultisigIsmConfig, DeployedIsmAddress>
  > {
    return noSingleMultisigIsm();
  }

  // The contract does not exist yet when the orchestrator asks for a new
  // origin's ISM. The zero-address sentinel keeps the validated config flowing
  // to the routing writer and, on a deploy, to the mailbox writer.
  async create(
    artifact: ArtifactNew<MultisigIsmConfig>,
  ): Promise<
    [ArtifactDeployed<MultisigIsmConfig, DeployedIsmAddress>, TxReceipt[]]
  > {
    resolveValidatorSet(artifact.config, await maxValidators());
    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: artifact.config,
        deployed: { address: ZERO_ADDRESS_HEX_32 },
      },
      [],
    ];
  }

  async update(): Promise<MidnightTransaction[]> {
    return [];
  }
}

class MidnightRoutingIsmReader implements ArtifactReader<
  RoutingIsmConfig,
  DeployedIsmAddress
> {
  constructor(protected readonly client: MidnightReadClient) {}

  // Night is its own ISM: the routing ISM and every origin's multisig carry
  // night's address, and the owner is night's owner commitment.
  async read(
    address: string,
  ): Promise<ArtifactDeployed<RoutingIsmConfig, DeployedIsmAddress>> {
    const { owner, sets } = await this.readValidatorSets(address);
    const domains: RoutingIsmConfig['domains'] = {};
    for (const [origin, config] of sets) {
      domains[origin] = {
        artifactState: ArtifactState.DEPLOYED,
        config,
        deployed: { address },
      };
    }
    return {
      artifactState: ArtifactState.DEPLOYED,
      config: { type: 'domainRoutingIsm', owner, domains },
      deployed: { address },
    };
  }

  protected async readValidatorSets(
    address: string,
  ): Promise<{ owner: string; sets: Map<number, MultisigIsmConfig> }> {
    const state = await this.client.requireContractState(address);
    const [moduleType, owner] = await Promise.all([
      this.client.runCircuit<bigint>('night', state.data, 'moduleType'),
      this.client.runCircuit<Uint8Array>('night', state.data, 'owner'),
    ]);
    if (moduleType !== MODULE_TYPE_MESSAGE_ID_MULTISIG) {
      throw new Error(
        `unexpected ISM module type ${moduleType} at ${address}, only messageIdMultisigIsm (5) exists on Midnight`,
      );
    }
    const sets = new Map<number, MultisigIsmConfig>();
    for (const origin of readValidatorSetOrigins(state.data)) {
      const set = await this.client.runCircuit<OnChainValidatorSet>(
        'night',
        state.data,
        'validatorSet',
        [BigInt(origin)],
      );
      // The chain stores full pubkeys; expose both them and the derived eth
      // addresses so a derived config round-trips into write-ready configs
      // (writes need the pubkeys, checks compare the addresses).
      const pubkeys = set.validators.slice(0, Number(set.count));
      sets.set(origin, {
        type: 'messageIdMultisigIsm',
        validators: pubkeys.map(pubkeyToEthAddress),
        threshold: Number(set.threshold),
        validatorPubkeys: pubkeys.map(bytesToHex),
      });
    }
    return { owner: bytesToHex(owner), sets };
  }
}

export class MidnightRoutingIsmWriter
  extends MidnightRoutingIsmReader
  implements ArtifactWriter<RoutingIsmConfig, DeployedIsmAddress>
{
  // The ISM is a facet of the night monolith and cannot deploy standalone: the
  // first origin's validators are constructor args. The zero-address sentinel
  // keeps the config flowing to the mailbox writer, which deploys night with
  // it and stamps the real address back here.
  async create(
    artifact: ArtifactNew<RoutingIsmConfig>,
  ): Promise<
    [ArtifactDeployed<RoutingIsmConfig, DeployedIsmAddress>, TxReceipt[]]
  > {
    if (validatorSetsByOrigin(artifact.config.domains).size === 0) {
      throw new Error(
        `night needs the validators of at least one origin chain: ${NIGHT_ISM_SHAPE}`,
      );
    }
    return [
      {
        artifactState: ArtifactState.DEPLOYED,
        config: artifact.config,
        deployed: { address: ZERO_ADDRESS_HEX_32 },
      },
      [],
    ];
  }

  // One full replace per origin whose set differs or is new, and one removal
  // per origin that is on chain but no longer in the config.
  async update(
    artifact: ArtifactDeployed<RoutingIsmConfig, DeployedIsmAddress>,
  ): Promise<MidnightTransaction[]> {
    const address = artifact.deployed.address;
    const expected = validatorSetsByOrigin(artifact.config.domains);
    // No origins would remove every validator set, and it is also what a
    // registry that knows none of the config's chains leaves of the config.
    if (expected.size === 0) {
      throw new Error(
        `night needs the validators of at least one origin chain: ${NIGHT_ISM_SHAPE}`,
      );
    }
    const current = await this.readValidatorSets(address);

    const expectedOwner = artifact.config.owner;
    if (
      expectedOwner &&
      expectedOwner !== ZERO_ADDRESS_HEX_32 &&
      normalizeHexForCompare(expectedOwner) !==
        normalizeHexForCompare(current.owner)
    ) {
      throw new Error(
        `night's ISM has no owner of its own: it is night's owner ` +
          `(${current.owner}), and the config says ${expectedOwner}. Move ` +
          `ownership through the core config's owner and write the ISM owner ` +
          `as the zero address`,
      );
    }

    const slots = await maxValidators();
    const txs: MidnightTransaction[] = [];
    for (const [origin, config] of expected) {
      const onChain = current.sets.get(origin);
      if (onChain && sameValidatorSet(onChain, config)) continue;
      txs.push(
        setValidatorsTx(address, origin, resolveValidatorSet(config, slots)),
      );
    }
    for (const origin of current.sets.keys()) {
      if (expected.has(origin)) continue;
      txs.push({
        annotation: `Remove the validators for origin ${origin} on ${address}`,
        contract: 'night',
        contractAddress: address,
        circuit: 'removeValidators',
        args: [BigInt(origin)],
      });
    }
    return txs;
  }
}

export class MidnightIsmArtifactManager implements IRawIsmArtifactManager {
  private readonly client: MidnightReadClient;

  constructor(chainMetadata: ChainMetadataForAltVM) {
    this.client = MidnightReadClient.fromMetadata(chainMetadata);
  }

  async readIsm(address: string): Promise<DeployedRawIsmArtifact> {
    return this.createReader('domainRoutingIsm').read(address);
  }

  createReader<T extends IsmType>(
    type: T,
  ): ArtifactReader<RawIsmArtifactConfigs[T], DeployedIsmAddress> {
    const readers: {
      [K in IsmType]: () => ArtifactReader<
        RawIsmArtifactConfigs[K],
        DeployedIsmAddress
      >;
    } = {
      messageIdMultisigIsm: noSingleMultisigIsm,
      merkleRootMultisigIsm: unsupportedOnMidnight(
        'ISM',
        'merkleRootMultisigIsm',
      ),
      domainRoutingIsm: () => new MidnightRoutingIsmReader(this.client),
      testIsm: unsupportedOnMidnight('ISM', 'testIsm'),
      compositeIsm: unsupportedOnMidnight('ISM', 'compositeIsm'),
    };
    return readers[type]();
  }

  createWriter<T extends IsmType>(
    type: T,
    _signer: ISigner<AnnotatedTx, TxReceipt>,
  ): ArtifactWriter<RawIsmArtifactConfigs[T], DeployedIsmAddress> {
    const writers: {
      [K in IsmType]: () => ArtifactWriter<
        RawIsmArtifactConfigs[K],
        DeployedIsmAddress
      >;
    } = {
      messageIdMultisigIsm: () => new MidnightMultisigIsmWriter(),
      merkleRootMultisigIsm: unsupportedOnMidnight(
        'ISM',
        'merkleRootMultisigIsm',
      ),
      domainRoutingIsm: () => new MidnightRoutingIsmWriter(this.client),
      testIsm: unsupportedOnMidnight('ISM', 'testIsm'),
      compositeIsm: unsupportedOnMidnight('ISM', 'compositeIsm'),
    };
    return writers[type]();
  }
}
