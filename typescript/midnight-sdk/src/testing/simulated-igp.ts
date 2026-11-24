import { SimulatedContract } from './simulated-contract.js';

// An IGP deployed the way the SDK deploys it, with a user address as the
// beneficiary.
export async function deployIgp(): Promise<SimulatedContract> {
  const igp = new SimulatedContract('igp');
  await igp.deploy([
    await igp.ownerId(),
    igp.instanceSalt,
    {
      is_left: false,
      left: { bytes: new Uint8Array(32) },
      right: { bytes: new Uint8Array(32).fill(4) },
    },
  ]);
  return igp;
}
