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
 * @title DeployCardanoISM
 * @notice Deploys a MultisigISM on Fuji for validating messages from Cardano
 * @dev Creates a simple 1-of-1 multisig with the Cardano validator
 */
contract DeployCardanoISM is Script {
    // Cardano domain ID
    uint32 constant CARDANO_DOMAIN = 2003;

    // Cardano validator address (derived from CARDANO_VALIDATOR_KEY)
    // Key: 0x2e0afff1080232cd5fc8fe769dd72f5766e4e0b66e5528fa93f80e75aca9e764
    address constant CARDANO_VALIDATOR =
        0x0A923108968Cf8427693679eeE7b98340Fe038ce;

    // Threshold for multisig (1 of 1)
    uint8 constant THRESHOLD = 1;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        console.log("Deploying Cardano MultisigISM on Fuji");
        console.log("Deployer:", deployer);
        console.log("Cardano Validator:", CARDANO_VALIDATOR);
        console.log("Threshold:", THRESHOLD);

        vm.startBroadcast(deployerPrivateKey);

        // Create validator set
        address[] memory validators = new address[](1);
        validators[0] = CARDANO_VALIDATOR;

        // Deploy StaticMerkleRootMultisigIsm for Cardano domain
        // Using the factory pattern for deterministic deployment
        StaticMerkleRootMultisigIsmFactory factory = new StaticMerkleRootMultisigIsmFactory();
        address ism = factory.deploy(validators, THRESHOLD);

        console.log("\n=== Deployment Complete ===");
        console.log("Factory deployed:", address(factory));
        console.log("MultisigISM deployed:", ism);
        console.log("\nThis ISM validates messages with:");
        console.log("  - Validator:", CARDANO_VALIDATOR);
        console.log("  - Threshold:", THRESHOLD);

        vm.stopBroadcast();

        console.log("\n=== Environment Variable ===");
        console.log(string.concat("FUJI_CARDANO_ISM=", vm.toString(ism)));
    }

    /**
     * @notice Set the new ISM on all Fuji warp routes
     */
    function setISMOnWarpRoutes() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");

        address cardanoIsm = vm.envAddress("FUJI_CARDANO_ISM");

        // Read warp route addresses
        address syntheticWCtest = vm.envAddress("FUJI_SYNTHETIC_WCTEST");
        address collateralFtest = vm.envAddress("FUJI_COLLATERAL_FTEST");
        address syntheticWAda = vm.envAddress("FUJI_SYNTHETIC_WADA");
        address collateralWada = vm.envAddress("FUJI_COLLATERAL_WADA");

        console.log("Setting Cardano ISM on Fuji warp routes");
        console.log("ISM:", cardanoIsm);

        vm.startBroadcast(deployerPrivateKey);

        // Set ISM on synthetic wCTEST (receives from Cardano collateral)
        ITokenRouter(syntheticWCtest).setInterchainSecurityModule(cardanoIsm);
        console.log("Set ISM on synthetic wCTEST:", syntheticWCtest);

        // Set ISM on collateral FTEST (receives from Cardano synthetic - burn/unlock)
        ITokenRouter(collateralFtest).setInterchainSecurityModule(cardanoIsm);
        console.log("Set ISM on collateral FTEST:", collateralFtest);

        // Set ISM on synthetic wADA (receives from Cardano native)
        ITokenRouter(syntheticWAda).setInterchainSecurityModule(cardanoIsm);
        console.log("Set ISM on synthetic wADA:", syntheticWAda);

        // Set ISM on collateral WADA (receives from Cardano native)
        ITokenRouter(collateralWada).setInterchainSecurityModule(cardanoIsm);
        console.log("Set ISM on collateral WADA:", collateralWada);

        vm.stopBroadcast();

        console.log("\n=== ISM Configuration Complete ===");
    }

    /**
     * @notice Deploy a TestRecipient on Fuji and set the Cardano ISM
     */
    function deployTestRecipient() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        address cardanoIsm = vm.envAddress("FUJI_CARDANO_ISM");

        console.log("Deploying TestRecipient on Fuji");
        console.log("Deployer:", deployer);
        console.log("Cardano ISM:", cardanoIsm);

        vm.startBroadcast(deployerPrivateKey);

        // Deploy TestRecipient
        TestRecipient recipient = new TestRecipient();

        // Set the Cardano ISM
        recipient.setInterchainSecurityModule(cardanoIsm);

        console.log("\n=== TestRecipient Deployed ===");
        console.log("Address:", address(recipient));
        console.log("ISM:", address(recipient.interchainSecurityModule()));

        vm.stopBroadcast();

        // Output H256 format for Cardano dispatch
        console.log("\n=== For Cardano dispatch ===");
        console.log(
            string.concat(
                "FUJI_TEST_RECIPIENT=",
                vm.toString(address(recipient))
            )
        );
        console.log(
            string.concat(
                "FUJI_TEST_RECIPIENT_H256=0x000000000000000000000000",
                vm.toString(address(recipient))
            )
        );
    }

    /**
     * @notice Set ISM on an existing TestRecipient
     */
    function setISMOnTestRecipient() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");

        address cardanoIsm = vm.envAddress("FUJI_CARDANO_ISM");
        address payable testRecipient = payable(
            vm.envAddress("FUJI_TEST_RECIPIENT")
        );

        console.log("Setting Cardano ISM on TestRecipient");
        console.log("TestRecipient:", testRecipient);
        console.log("ISM:", cardanoIsm);

        vm.startBroadcast(deployerPrivateKey);

        TestRecipient(testRecipient).setInterchainSecurityModule(cardanoIsm);

        console.log("\n=== ISM Set Successfully ===");

        vm.stopBroadcast();
    }
}
