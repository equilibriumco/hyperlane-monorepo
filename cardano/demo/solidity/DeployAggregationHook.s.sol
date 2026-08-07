// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {StaticAggregationHookFactory} from "contracts/hooks/aggregation/StaticAggregationHookFactory.sol";

/**
 * @title DeployAggregationHook
 * @notice Deploys a StaticAggregationHook combining MerkleTreeHook + IGP.
 *
 *         Warp route contracts on the EVM side MUST use an AggregationHook
 *         (not just an IGP) so that dispatched messages are inserted into
 *         the MerkleTreeHook. Without the MerkleTreeHook insertion,
 *         validators cannot sign checkpoints covering the message and
 *         cross-chain delivery will fail.
 *
 * Required environment variables:
 *   - EVM_SIGNER_KEY: Private key for transactions
 *   - EVM_MERKLE_TREE_HOOK: MerkleTreeHook address
 *   - EVM_IGP: InterchainGasPaymaster address
 */
contract DeployAggregationHook is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("EVM_SIGNER_KEY");
        address merkleTreeHook = vm.envAddress("EVM_MERKLE_TREE_HOOK");
        address igp = vm.envAddress("EVM_IGP");

        console.log("Deploying StaticAggregationHook");
        console.log("  MerkleTreeHook:", merkleTreeHook);
        console.log("  IGP:", igp);

        vm.startBroadcast(deployerPrivateKey);

        StaticAggregationHookFactory factory = new StaticAggregationHookFactory();
        console.log("  Factory deployed:", address(factory));

        address[] memory hooks = new address[](2);
        hooks[0] = merkleTreeHook;
        hooks[1] = igp;

        address aggregationHook = factory.deploy(hooks);
        console.log("  AggregationHook deployed:", aggregationHook);

        vm.stopBroadcast();

        console.log("\n=== Environment Variables ===");
        console.log(
            string.concat("EVM_AGGREGATION_HOOK=", vm.toString(aggregationHook))
        );
    }
}
