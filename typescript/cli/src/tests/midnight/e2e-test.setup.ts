import fs from 'fs';

import {
  CHAIN_1_ADDRESSES_PATH,
  CHAIN_1_METADATA_PATH,
  MIDNIGHT_STATE_DIR,
  REGISTRY_PATH,
  TEMP_PATH,
} from './consts.js';

// `core deploy` writes the deploy block into the chain metadata as
// `index.from`; the tracked file is put back once the run ends.
let chainMetadata: string | undefined;

// The devnet is externally managed and run-e2e-test.sh checks it is reachable
// before mocha starts, so this only resets on-disk state.
before(function () {
  chainMetadata = fs.readFileSync(CHAIN_1_METADATA_PATH, 'utf8');

  if (fs.existsSync(CHAIN_1_ADDRESSES_PATH)) {
    fs.rmSync(CHAIN_1_ADDRESSES_PATH, { force: true });
  }

  // Owner nonces and maintenance signing keys from previous runs belong to
  // previous instances; a fresh deploy must start from a fresh store.
  if (fs.existsSync(MIDNIGHT_STATE_DIR)) {
    fs.rmSync(MIDNIGHT_STATE_DIR, { recursive: true, force: true });
  }
  fs.mkdirSync(MIDNIGHT_STATE_DIR, { recursive: true });
  process.env.MIDNIGHT_STATE_DIR = MIDNIGHT_STATE_DIR;

  fs.mkdirSync(`${TEMP_PATH}/midnight1`, { recursive: true });
});

after(function () {
  if (chainMetadata !== undefined) {
    fs.writeFileSync(CHAIN_1_METADATA_PATH, chainMetadata);
  }
});

// Reset the warp route deployments for each test invocation
beforeEach(() => {
  const deploymentPaths = `${REGISTRY_PATH}/deployments/warp_routes`;

  if (fs.existsSync(deploymentPaths)) {
    fs.rmSync(deploymentPaths, { recursive: true, force: true });
  }
});
