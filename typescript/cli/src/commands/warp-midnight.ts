import { type CommandModule } from 'yargs';

import {
  MidnightProvider,
  requireMidnightSigner,
} from '@hyperlane-xyz/midnight-sdk';
import { ProtocolType, assert, mustGet } from '@hyperlane-xyz/utils';

import {
  type CommandContext,
  type CommandModuleWithContext,
  type CommandModuleWithWriteContext,
} from '../context/types.js';
import { log, logCommandHeader, logGreen } from '../logger.js';

import { addressCommandOption, chainCommandOption } from './options.js';

// The night contract is both the mailbox and the warp token of a Midnight
// route, so these commands act on a chain's night, not on a route.
const nightOptions = {
  chain: {
    ...chainCommandOption,
    description: 'The Midnight chain night is deployed on',
    demandOption: true,
  },
  address: addressCommandOption(
    'Address of the night contract. Defaults to the chain mailbox in the registry',
  ),
} as const;

async function resolveNight(
  context: CommandContext,
  chain: string,
  address?: string,
): Promise<string> {
  assert(
    context.multiProvider.getProtocol(chain) === ProtocolType.Midnight,
    `${chain} is not a Midnight chain`,
  );
  if (address) return address;
  const mailbox = (await context.registry.getChainAddresses(chain))?.mailbox;
  assert(
    mailbox,
    `No mailbox address for ${chain} in the registry, pass --address`,
  );
  return mailbox;
}

const pause: CommandModuleWithWriteContext<{
  chain: string;
  address?: string;
}> = {
  command: 'pause',
  describe:
    'Pause night: deliveries, transfers out and funding stop until it is unpaused',
  builder: nightOptions,
  handler: async ({ context, chain, address }) => {
    logCommandHeader('Hyperlane Warp Midnight Pause');
    const night = await resolveNight(context, chain, address);
    const signer = requireMidnightSigner(mustGet(context.altVmSigners, chain));
    const { txId } = await signer.pauseNight(night);
    logGreen(`night ${night} paused in tx ${txId}`);
    process.exit(0);
  },
};

const unpause: CommandModuleWithWriteContext<{
  chain: string;
  address?: string;
}> = {
  command: 'unpause',
  describe: 'Unpause night',
  builder: nightOptions,
  handler: async ({ context, chain, address }) => {
    logCommandHeader('Hyperlane Warp Midnight Unpause');
    const night = await resolveNight(context, chain, address);
    const signer = requireMidnightSigner(mustGet(context.altVmSigners, chain));
    const { txId } = await signer.unpauseNight(night);
    logGreen(`night ${night} unpaused in tx ${txId}`);
    process.exit(0);
  },
};

const fund: CommandModuleWithWriteContext<{
  chain: string;
  address?: string;
  amount: string;
}> = {
  command: 'fund',
  describe:
    "Move NIGHT from the owner's wallet into night's escrow, which deliveries into Midnight pay out of",
  builder: {
    ...nightOptions,
    amount: {
      type: 'string',
      description: 'Amount in micro-NIGHT (1000000 is 1 NIGHT)',
      demandOption: true,
    },
  },
  handler: async ({ context, chain, address, amount }) => {
    logCommandHeader('Hyperlane Warp Midnight Fund');
    assert(
      /^[1-9][0-9]*$/.test(amount),
      `--amount must be a positive whole number of micro-NIGHT, got ${amount}`,
    );
    const night = await resolveNight(context, chain, address);
    const signer = requireMidnightSigner(mustGet(context.altVmSigners, chain));
    const { txId } = await signer.fundNight(night, BigInt(amount));
    logGreen(`${amount} micro-NIGHT moved into night ${night} in tx ${txId}`);
    process.exit(0);
  },
};

const status: CommandModuleWithContext<{
  chain: string;
  address?: string;
}> = {
  command: 'status',
  describe: 'Show whether night is paused and what its escrow holds',
  builder: nightOptions,
  handler: async ({ context, chain, address }) => {
    logCommandHeader('Hyperlane Warp Midnight Status');
    const night = await resolveNight(context, chain, address);
    const provider = mustGet(context.altVmProviders, chain);
    assert(
      provider instanceof MidnightProvider,
      `Expected a Midnight provider for chain ${chain}`,
    );
    const [paused, escrow] = await Promise.all([
      provider.isPaused(night),
      provider.getBridgedSupply({ tokenAddress: night }),
    ]);
    log(`night ${night}`);
    log(`  paused: ${paused ? 'yes' : 'no'}`);
    log(`  escrow: ${escrow} micro-NIGHT`);
    process.exit(0);
  },
};

export const midnightCommand: CommandModule = {
  command: 'midnight',
  describe: 'Pause, unpause and fund the night contract of a Midnight route',
  builder: (yargs) =>
    yargs
      .command(fund)
      .command(pause)
      .command(status)
      .command(unpause)
      .version(false)
      .demandCommand(),
  handler: () => log('Command required'),
};
