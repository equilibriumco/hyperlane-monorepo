import fs from 'fs';

import {
  CHAIN_1_ADDRESSES_PATH,
  MIDNIGHT_STATE_DIR,
  REGISTRY_PATH,
  TEMP_PATH,
} from './consts.js';

// The devnet itself (node + indexer + proof server) is externally managed;
// run-e2e-test.sh verifies it is reachable before mocha starts. This file
// only resets the on-disk state so every run begins with a fresh deploy.
before(function () {
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

// Reset the warp route deployments for each test invocation
beforeEach(() => {
  const deploymentPaths = `${REGISTRY_PATH}/deployments/warp_routes`;

  if (fs.existsSync(deploymentPaths)) {
    fs.rmSync(deploymentPaths, { recursive: true, force: true });
  }
});
