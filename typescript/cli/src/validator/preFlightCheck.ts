import { MerkleTreeHook__factory } from '@hyperlane-xyz/core';
import { HyperlaneCore, S3Validator } from '@hyperlane-xyz/sdk';
import { type Address, ProtocolType } from '@hyperlane-xyz/utils';

import { type CommandContext } from '../context/types.js';
import { errorRed, logBlue, logGreen, warnYellow } from '../logger.js';

// Storage locations and the latest checkpoint index, per protocol. Midnight's
// merkle tree lives off-chain, so the mailbox nonce stands in for the tree size.
async function getOnChainValidatorState(
  context: CommandContext,
  chain: string,
  validatorsArray: Address[],
): Promise<{
  latestCheckpointIndex: number | undefined;
  storageLocations: string[][] | undefined;
  errors: string[];
}> {
  const { multiProvider, registry } = context;
  const addresses = await registry.getAddresses();
  const errors: string[] = [];

  if (
    multiProvider.tryGetChainMetadata(chain)?.protocol === ProtocolType.Midnight
  ) {
    const { MidnightReadClient, MidnightValidatorAnnounceArtifactManager } =
      await import('@hyperlane-xyz/midnight-sdk');
    const metadata = multiProvider.getChainMetadata(chain);

    let latestCheckpointIndex: number | undefined;
    try {
      const client = MidnightReadClient.fromMetadata(metadata);
      const state = await client.requireContractState(addresses[chain].mailbox);
      const nonce = await client.runCircuit<bigint>(
        'night',
        state.data,
        'nonceValue',
      );
      latestCheckpointIndex = nonce > 0n ? Number(nonce - 1n) : undefined;
    } catch (err) {
      warnYellow(`❗️ Failed to read the mailbox nonce on ${chain}: ${err} \n`);
    }

    let storageLocations: string[][] | undefined;
    try {
      const vaManager = new MidnightValidatorAnnounceArtifactManager(metadata);
      storageLocations = await vaManager.getAnnouncedStorageLocations(
        addresses[chain].validatorAnnounce,
        validatorsArray,
      );
    } catch {
      errors.push('Failed to read announced storage locations on chain.');
    }
    return { latestCheckpointIndex, storageLocations, errors };
  }

  const core = HyperlaneCore.fromAddressesMap(addresses, multiProvider);
  const validatorAnnounce = core.getContracts(chain).validatorAnnounce;
  const merkleTreeHook = MerkleTreeHook__factory.connect(
    addresses[chain].merkleTreeHook,
    multiProvider.getProvider(chain),
  );

  let latestCheckpointIndex: number | undefined;
  try {
    const [_, checkpointIndex] = await merkleTreeHook.latestCheckpoint();
    latestCheckpointIndex = checkpointIndex;
  } catch (err) {
    warnYellow(
      `❗️ Failed to fetch latest checkpoint index of merkleTreeHook on ${chain}: ${err} \n`,
    );
  }

  let storageLocations: string[][] | undefined;
  try {
    storageLocations =
      await validatorAnnounce.getAnnouncedStorageLocations(validatorsArray);
  } catch {
    errors.push('Failed to read announced storage locations on chain.');
  }

  return { latestCheckpointIndex, storageLocations, errors };
}

export const checkValidatorSetup = async (
  context: CommandContext,
  chain: string,
  validators: Set<Address>,
) => {
  const errorSet = new Set<string>();
  const validatorsArray = Array.from(validators);

  const {
    latestCheckpointIndex: merkleTreeLatestCheckpointIndex,
    storageLocations: validatorStorageLocations,
    errors,
  } = await getOnChainValidatorState(context, chain, validatorsArray);
  errors.forEach((e) => errorSet.add(e));

  if (merkleTreeLatestCheckpointIndex !== undefined) {
    logBlue(
      `\nLatest checkpoint index of incremental merkle tree: ${merkleTreeLatestCheckpointIndex}\n`,
    );
  }

  if (validatorStorageLocations) {
    for (let i = 0; i < validatorsArray.length; i++) {
      const validator = validatorsArray[i];
      const storageLocations = validatorStorageLocations[i];

      if (storageLocations.length === 0) {
        errorRed(`❌ Validator ${validator} has not been announced\n`);
        errorSet.add('Some validators have not been announced.');
        continue;
      }

      const s3StorageLocation = storageLocations[0];

      let s3Validator: S3Validator;
      try {
        s3Validator = await S3Validator.fromStorageLocation(s3StorageLocation);
      } catch {
        errorRed(
          `❌ Failed to fetch storage locations for validator ${validator}, this may be due to the storage location not being an S3 bucket\n\n`,
        );
        errorSet.add('Failed to fetch storage locations for some validators.');
        continue;
      }

      const latestCheckpointIndex =
        await s3Validator.getLatestCheckpointIndex();

      logBlue(
        `✅ Validator ${validator} announced\nstorage location: ${s3StorageLocation}\nlatest checkpoint index: ${latestCheckpointIndex}`,
      );

      // check is latestCheckpointIndex is within 1% of the merkleTreeLatestCheckpointIndex
      if (merkleTreeLatestCheckpointIndex) {
        const diff = Math.abs(
          latestCheckpointIndex - merkleTreeLatestCheckpointIndex,
        );
        if (diff > merkleTreeLatestCheckpointIndex / 100) {
          errorRed(
            `❌ Validator is not signing the latest available checkpoint\n\n`,
          );
          errorSet.add(
            `Some validators are not signing the latest available checkpoint`,
          );
        } else {
          logBlue(
            `✅ Validator is signing the latest available checkpoint\n\n`,
          );
        }
      } else {
        warnYellow(
          `❗️ Cannot compare validator checkpoint signatures to latest checkpoint in the incremental merkletree, merkletree checkpoint could not be read\n`,
        );
      }
    }
  }

  if (errorSet.size > 0) {
    errorRed(
      `\n❌ Validator pre flight check failed:\n${Array.from(errorSet).join(
        '\n',
      )}`,
    );
    process.exit(1);
  } else {
    logGreen(`\n✅ Validator pre flight check passed`);
  }
};
