export const E2E_TEST_CONFIGS_PATH = './test-configs';
export const REGISTRY_PATH = `${E2E_TEST_CONFIGS_PATH}/test-registry`;
export const TEMP_PATH = '/tmp';

// The upstream Midnight `dev` preset master seed: it holds the minted NIGHT
// supply on a local devnet. Local development only.
export const HYP_KEY =
  '0000000000000000000000000000000000000000000000000000000000000001';

export const EXAMPLES_PATH = './examples/midnight';

export const CHAIN_NAME_1 = 'midnight1';

export const CHAIN_1_METADATA_PATH = `${REGISTRY_PATH}/chains/${CHAIN_NAME_1}/metadata.yaml`;
export const CHAIN_1_ADDRESSES_PATH = `${REGISTRY_PATH}/chains/${CHAIN_NAME_1}/addresses.yaml`;

export const CORE_CONFIG_PATH = `${EXAMPLES_PATH}/core-config.yaml`;
export const CORE_READ_CONFIG_PATH_1 = `${TEMP_PATH}/${CHAIN_NAME_1}/core-config-read.yaml`;
export const HOOK_READ_CONFIG_PATH_1 = `${TEMP_PATH}/${CHAIN_NAME_1}/hook-config-read.yaml`;
export const HOOK_APPLY_CONFIG_PATH_1 = `${TEMP_PATH}/${CHAIN_NAME_1}/hook-config-apply.yaml`;
export const WARP_READ_OUTPUT_PATH_1 = `${TEMP_PATH}/${CHAIN_NAME_1}/warp-config-read.yaml`;

// The signer's private state (owner nonces, maintenance signing keys) lives
// here for the duration of a test run; e2e-test.setup.ts resets it together
// with the chain addresses so every run starts from a fresh deploy.
export const MIDNIGHT_STATE_DIR = `${TEMP_PATH}/${CHAIN_NAME_1}/state`;

// A remote domain for router-enrollment and gas-oracle entries. The chain
// only has to exist in the test registry — nothing is ever submitted to it.
export const REMOTE_CHAIN_NAME = 'anvil2';
export const REMOTE_DOMAIN_ID = 31338;

// Anvil dev account #0 as a 32-byte hex router address on the remote chain.
export const REMOTE_ROUTER_ADDRESS =
  '0x000000000000000000000000f39fd6e51aad88f6f4ce6ab8827279cfffb92266';

// Ethereum key for the remote chain entry in warp deploy configs: chains
// marked foreignDeployment are never written to, but the signer middleware
// still requires a key for them. Anvil dev key #0.
export const REMOTE_HYP_KEY =
  '0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80';

// Every state change on Midnight is a real proof: wallet balancing proofs
// for deploys, circuit proofs for owner-gated updates. Minutes each, not
// seconds — hence the dedicated timeout.
export const MIDNIGHT_E2E_TEST_TIMEOUT = 1_200_000;
