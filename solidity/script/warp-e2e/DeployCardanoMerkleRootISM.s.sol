// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {StaticMerkleRootMultisigIsm, StaticMerkleRootMultisigIsmFactory} from "contracts/isms/multisig/StaticMultisigIsm.sol";
import {TestRecipient} from "contracts/test/TestRecipient.sol";

interface ITokenRouter {
    function setInterchainSecurityModule(address _module) external;
    function interchainSecurityModule() external view returns (address);
}

/**
 * @title DeployCardanoMerkleRootISM
 * @notice Deploys a MerkleRoot MultisigISM on Sepolia for validating messages from
 *         Cardano. Unlike the MessageId variant, the relayer supplies a merkle
 *         inclusion proof of the message against a validator-signed root, which
 *         is resilient to per-index checkpoint gaps.
 *
 * Required env:
 *   - EVM_SIGNER_KEY: Private key for Sepolia transactions
 *   - CARDANO_VALIDATOR: Cardano validator address (20-byte EVM address)
 * Optional:
 *   - CARDANO_ISM_THRESHOLD (default 1)
 */
contract DeployCardanoMerkleRootISM is Script {
    uint8 constant DEFAULT_THRESHOLD = 1;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("EVM_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);
        address cardanoValidator = vm.envAddress("CARDANO_VALIDATOR");
        uint8 threshold = uint8(
            vm.envOr("CARDANO_ISM_THRESHOLD", uint256(DEFAULT_THRESHOLD))
        );

        console.log("Deploying Cardano MerkleRoot MultisigISM on Sepolia");
        console.log("Deployer:", deployer);
        console.log("Cardano Validator:", cardanoValidator);
        console.log("Threshold:", threshold);

        vm.startBroadcast(deployerPrivateKey);

        address[] memory validators = new address[](1);
        validators[0] = cardanoValidator;

        StaticMerkleRootMultisigIsmFactory factory = new StaticMerkleRootMultisigIsmFactory();
        address ism = factory.deploy(validators, threshold);

        vm.stopBroadcast();

        console.log("Factory:", address(factory));
        console.log("MerkleRoot MultisigISM:", ism);
        console.log(string.concat("EVM_ISM=", vm.toString(ism)));
    }

    /// @notice Point an existing TestRecipient at the MerkleRoot ISM.
    function setISMOnTestRecipient() external {
        uint256 deployerPrivateKey = vm.envUint("EVM_SIGNER_KEY");
        address cardanoIsm = vm.envAddress("EVM_ISM");
        address payable testRecipient = payable(
            vm.envAddress("EVM_TEST_RECIPIENT")
        );

        vm.startBroadcast(deployerPrivateKey);
        TestRecipient(testRecipient).setInterchainSecurityModule(cardanoIsm);
        vm.stopBroadcast();

        console.log("Set MerkleRoot ISM on TestRecipient:", testRecipient);
        console.log("ISM:", cardanoIsm);
    }

    /// @notice Point the Sepolia warp routes at the MerkleRoot ISM.
    function setISMOnWarpRoutes() external {
        uint256 deployerPrivateKey = vm.envUint("EVM_SIGNER_KEY");
        address cardanoIsm = vm.envAddress("EVM_ISM");

        address[4] memory routes = [
            vm.envAddress("EVM_SYNTHETIC_WCTEST"),
            vm.envAddress("EVM_COLLATERAL_FTEST"),
            vm.envAddress("EVM_SYNTHETIC_WADA"),
            vm.envAddress("EVM_COLLATERAL_WADA")
        ];

        vm.startBroadcast(deployerPrivateKey);
        for (uint256 i = 0; i < routes.length; i++) {
            ITokenRouter(routes[i]).setInterchainSecurityModule(cardanoIsm);
            console.log("Set MerkleRoot ISM on route:", routes[i]);
        }
        vm.stopBroadcast();
    }
}
