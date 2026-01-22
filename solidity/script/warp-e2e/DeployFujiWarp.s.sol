// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {HypERC20} from "contracts/token/HypERC20.sol";
import {HypERC20Collateral} from "contracts/token/HypERC20Collateral.sol";
import {HypNative} from "contracts/token/HypNative.sol";
import {TestERC20} from "./TestERC20.sol";
import {TypeCasts} from "contracts/libs/TypeCasts.sol";

/**
 * @title DeployFujiWarp
 * @notice Deploys all warp route contracts on Fuji for E2E testing with Cardano
 * @dev Deploys contracts for all 7 test scenarios:
 *      1. Cardano Collateral <-> Fuji Synthetic (CTEST -> wCTEST)
 *      2. Fuji Collateral <-> Cardano Synthetic (FTEST -> wFTEST)
 *      3. Cardano Native <-> Fuji Synthetic (ADA -> wADA)
 *      4. Fuji Native <-> Cardano Synthetic (AVAX -> wAVAX)
 *      5. Cardano Native <-> Fuji Collateral (ADA -> WADA ERC20)
 *      6. Fuji Native <-> Cardano Collateral (AVAX -> WAVAX token)
 *      7. Collateral <-> Collateral (TokenA <-> TokenB)
 */
contract DeployFujiWarp is Script {
    using TypeCasts for address;

    // Fuji Hyperlane infrastructure
    address constant FUJI_MAILBOX = 0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0;
    address constant FUJI_ISM = 0xD44036F1917bb13cB36a4ab1ad0F87324aacF1EB;

    // Cardano domain ID
    uint32 constant CARDANO_DOMAIN = 2003;

    // Scale factors for decimal conversion
    // Cardano has 6 decimals, EVM has 18 decimals
    // Scale = 10^(18-6) = 10^12 for Cardano -> EVM
    uint256 constant SCALE_6_TO_18 = 1e12;
    // No scaling needed for same decimals
    uint256 constant SCALE_18_TO_18 = 1;

    struct DeployedContracts {
        // Test tokens
        address ftest; // Fuji test token (for scenario 2 collateral)
        address wada; // Wrapped ADA ERC20 (for scenario 5 collateral)
        address tokenA; // Token A for collateral-collateral test
        // Synthetic warp routes
        address syntheticWCtest; // Scenario 1: receives CTEST, mints wCTEST
        address syntheticWAda; // Scenario 3: receives ADA, mints wADA
        address syntheticWAvax; // Scenario 4: for Cardano to receive wAVAX
        // Collateral warp routes
        address collateralFtest; // Scenario 2: locks FTEST
        address collateralWada; // Scenario 5: releases pre-deposited WADA
        address collateralTokenA; // Scenario 7: collateral-collateral TokenA side
        // Native warp route
        address nativeAvax; // Scenario 4 & 6: locks AVAX
    }

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        console.log("Deploying Fuji Warp Routes");
        console.log("Deployer:", deployer);
        console.log("Mailbox:", FUJI_MAILBOX);
        console.log("ISM:", FUJI_ISM);

        vm.startBroadcast(deployerPrivateKey);

        DeployedContracts memory contracts;

        // ========== Deploy Test ERC20 Tokens ==========
        console.log("\n=== Deploying Test ERC20 Tokens ===");

        // FTEST - Fuji test token for collateral scenario 2
        contracts.ftest = address(
            new TestERC20("Fuji Test Token", "FTEST", 18)
        );
        console.log("FTEST deployed:", contracts.ftest);

        // WADA - Wrapped ADA ERC20 for native-collateral scenario 5
        contracts.wada = address(new TestERC20("Wrapped ADA", "WADA", 18));
        console.log("WADA deployed:", contracts.wada);

        // TokenA - For collateral-collateral scenario 7
        contracts.tokenA = address(new TestERC20("Token A", "TOKA", 18));
        console.log("TokenA deployed:", contracts.tokenA);

        // ========== Deploy Synthetic Warp Routes ==========
        console.log("\n=== Deploying Synthetic Warp Routes ===");

        // Scenario 1: Synthetic wCTEST (receives CTEST from Cardano)
        // Cardano collateral has 6 decimals, Fuji synthetic has 18
        contracts.syntheticWCtest = _deploySynthetic(
            6, // decimals (matches Cardano token)
            SCALE_6_TO_18, // scale up from 6 to 18
            "Wrapped CTEST",
            "wCTEST",
            deployer
        );
        console.log("Synthetic wCTEST deployed:", contracts.syntheticWCtest);

        // Scenario 3: Synthetic wADA (receives ADA from Cardano)
        // Cardano ADA has 6 decimals (lovelace)
        contracts.syntheticWAda = _deploySynthetic(
            6, // decimals (matches ADA)
            SCALE_6_TO_18, // scale up from 6 to 18
            "Wrapped ADA",
            "wADA",
            deployer
        );
        console.log("Synthetic wADA deployed:", contracts.syntheticWAda);

        // Scenario 4: Synthetic wAVAX (for Cardano to receive)
        // This is deployed on Cardano side, but we track it here for router enrollment
        // Actually this should be a native route on Fuji that locks AVAX
        // The synthetic is on Cardano

        // ========== Deploy Collateral Warp Routes ==========
        console.log("\n=== Deploying Collateral Warp Routes ===");

        // Scenario 2: Collateral FTEST (locks FTEST, Cardano receives synthetic)
        contracts.collateralFtest = _deployCollateral(
            contracts.ftest,
            SCALE_18_TO_18, // no scaling within EVM
            deployer
        );
        console.log("Collateral FTEST deployed:", contracts.collateralFtest);

        // Scenario 5: Collateral WADA (releases pre-deposited WADA for Cardano ADA)
        contracts.collateralWada = _deployCollateral(
            contracts.wada,
            SCALE_6_TO_18, // scale from Cardano 6 decimals to EVM 18
            deployer
        );
        console.log("Collateral WADA deployed:", contracts.collateralWada);

        // Scenario 7: Collateral TokenA (collateral-collateral)
        contracts.collateralTokenA = _deployCollateral(
            contracts.tokenA,
            SCALE_18_TO_18, // no scaling within EVM
            deployer
        );
        console.log("Collateral TokenA deployed:", contracts.collateralTokenA);

        // ========== Deploy Native Warp Route ==========
        console.log("\n=== Deploying Native Warp Route ===");

        // Scenarios 4 & 6: Native AVAX (locks AVAX)
        contracts.nativeAvax = _deployNative(
            SCALE_18_TO_18, // AVAX has 18 decimals
            deployer
        );
        console.log("Native AVAX deployed:", contracts.nativeAvax);

        vm.stopBroadcast();

        // ========== Output Summary ==========
        console.log("\n=== Deployment Summary ===");
        console.log("Test Tokens:");
        console.log("  FTEST:", contracts.ftest);
        console.log("  WADA:", contracts.wada);
        console.log("  TokenA:", contracts.tokenA);
        console.log("\nSynthetic Warp Routes:");
        console.log("  wCTEST (scenario 1):", contracts.syntheticWCtest);
        console.log("  wADA (scenario 3):", contracts.syntheticWAda);
        console.log("\nCollateral Warp Routes:");
        console.log("  FTEST (scenario 2):", contracts.collateralFtest);
        console.log("  WADA (scenario 5):", contracts.collateralWada);
        console.log("  TokenA (scenario 7):", contracts.collateralTokenA);
        console.log("\nNative Warp Route:");
        console.log("  AVAX (scenarios 4,6):", contracts.nativeAvax);

        // Output in a format easy to parse for scripts
        console.log("\n=== Environment Variables ===");
        console.log(string.concat("FUJI_FTEST=", vm.toString(contracts.ftest)));
        console.log(string.concat("FUJI_WADA=", vm.toString(contracts.wada)));
        console.log(
            string.concat("FUJI_TOKENA=", vm.toString(contracts.tokenA))
        );
        console.log(
            string.concat(
                "FUJI_SYNTHETIC_WCTEST=",
                vm.toString(contracts.syntheticWCtest)
            )
        );
        console.log(
            string.concat(
                "FUJI_SYNTHETIC_WADA=",
                vm.toString(contracts.syntheticWAda)
            )
        );
        console.log(
            string.concat(
                "FUJI_COLLATERAL_FTEST=",
                vm.toString(contracts.collateralFtest)
            )
        );
        console.log(
            string.concat(
                "FUJI_COLLATERAL_WADA=",
                vm.toString(contracts.collateralWada)
            )
        );
        console.log(
            string.concat(
                "FUJI_COLLATERAL_TOKENA=",
                vm.toString(contracts.collateralTokenA)
            )
        );
        console.log(
            string.concat(
                "FUJI_NATIVE_AVAX=",
                vm.toString(contracts.nativeAvax)
            )
        );
    }

    function _deploySynthetic(
        uint8 decimals,
        uint256 scale,
        string memory name,
        string memory symbol,
        address owner
    ) internal returns (address) {
        HypERC20 synthetic = new HypERC20(decimals, scale, FUJI_MAILBOX);
        synthetic.initialize(
            0, // no initial supply (minted on receive)
            name,
            symbol,
            address(0), // no hook
            FUJI_ISM, // ISM
            owner
        );
        return address(synthetic);
    }

    function _deployCollateral(
        address token,
        uint256 scale,
        address owner
    ) internal returns (address) {
        HypERC20Collateral collateral = new HypERC20Collateral(
            token,
            scale,
            FUJI_MAILBOX
        );
        collateral.initialize(
            address(0), // no hook
            FUJI_ISM, // ISM
            owner
        );
        return address(collateral);
    }

    function _deployNative(
        uint256 scale,
        address owner
    ) internal returns (address) {
        HypNative native = new HypNative(scale, FUJI_MAILBOX);
        native.initialize(
            address(0), // no hook
            FUJI_ISM, // ISM
            owner
        );
        return address(native);
    }

    /**
     * @notice Enroll remote routers for all warp routes
     * @dev Call this after Cardano warp routes are deployed
     */
    function enrollRouters() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");

        // Read deployed addresses from environment
        address syntheticWCtest = vm.envAddress("FUJI_SYNTHETIC_WCTEST");
        address syntheticWAda = vm.envAddress("FUJI_SYNTHETIC_WADA");
        address collateralFtest = vm.envAddress("FUJI_COLLATERAL_FTEST");
        address collateralWada = vm.envAddress("FUJI_COLLATERAL_WADA");
        address collateralTokenA = vm.envAddress("FUJI_COLLATERAL_TOKENA");
        address nativeAvax = vm.envAddress("FUJI_NATIVE_AVAX");

        // Read Cardano router addresses (as bytes32 with 0x00000000 prefix)
        bytes32 cardanoCollateralCtest = vm.envBytes32(
            "CARDANO_COLLATERAL_CTEST"
        );
        bytes32 cardanoSyntheticFtest = vm.envBytes32(
            "CARDANO_SYNTHETIC_FTEST"
        );
        bytes32 cardanoNativeAda = vm.envBytes32("CARDANO_NATIVE_ADA");
        bytes32 cardanoSyntheticAvax = vm.envBytes32("CARDANO_SYNTHETIC_AVAX");
        bytes32 cardanoCollateralWavax = vm.envBytes32(
            "CARDANO_COLLATERAL_WAVAX"
        );
        bytes32 cardanoCollateralTokenB = vm.envBytes32(
            "CARDANO_COLLATERAL_TOKENB"
        );

        vm.startBroadcast(deployerPrivateKey);

        console.log("Enrolling remote routers on Fuji warp routes...");

        // Scenario 1: wCTEST synthetic -> Cardano collateral CTEST
        HypERC20(syntheticWCtest).enrollRemoteRouter(
            CARDANO_DOMAIN,
            cardanoCollateralCtest
        );
        console.log(
            "Enrolled Cardano collateral CTEST as router for wCTEST synthetic"
        );

        // Scenario 2: FTEST collateral -> Cardano synthetic FTEST
        HypERC20Collateral(collateralFtest).enrollRemoteRouter(
            CARDANO_DOMAIN,
            cardanoSyntheticFtest
        );
        console.log(
            "Enrolled Cardano synthetic FTEST as router for FTEST collateral"
        );

        // Scenario 3: wADA synthetic -> Cardano native ADA
        HypERC20(syntheticWAda).enrollRemoteRouter(
            CARDANO_DOMAIN,
            cardanoNativeAda
        );
        console.log("Enrolled Cardano native ADA as router for wADA synthetic");

        // Scenario 4: AVAX native -> Cardano synthetic AVAX
        HypNative(payable(nativeAvax)).enrollRemoteRouter(
            CARDANO_DOMAIN,
            cardanoSyntheticAvax
        );
        console.log(
            "Enrolled Cardano synthetic AVAX as router for AVAX native"
        );

        // Scenario 5: WADA collateral -> Cardano native ADA
        HypERC20Collateral(collateralWada).enrollRemoteRouter(
            CARDANO_DOMAIN,
            cardanoNativeAda
        );
        console.log(
            "Enrolled Cardano native ADA as router for WADA collateral"
        );

        // Scenario 6: AVAX native -> Cardano collateral WAVAX
        // Note: Native AVAX is already enrolled for scenario 4, we need separate deployment
        // For now, reuse the same native contract (both scenarios use same AVAX lock mechanism)

        // Scenario 7: TokenA collateral -> Cardano collateral TokenB
        HypERC20Collateral(collateralTokenA).enrollRemoteRouter(
            CARDANO_DOMAIN,
            cardanoCollateralTokenB
        );
        console.log(
            "Enrolled Cardano collateral TokenB as router for TokenA collateral"
        );

        vm.stopBroadcast();
    }

    /**
     * @notice Mint test tokens to the deployer for testing
     */
    function mintTestTokens() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        address ftest = vm.envAddress("FUJI_FTEST");
        address wada = vm.envAddress("FUJI_WADA");
        address tokenA = vm.envAddress("FUJI_TOKENA");

        vm.startBroadcast(deployerPrivateKey);

        uint256 mintAmount = 1000000 * 1e18; // 1M tokens

        TestERC20(ftest).mint(deployer, mintAmount);
        console.log("Minted 1M FTEST to", deployer);

        TestERC20(wada).mint(deployer, mintAmount);
        console.log("Minted 1M WADA to", deployer);

        TestERC20(tokenA).mint(deployer, mintAmount);
        console.log("Minted 1M TokenA to", deployer);

        vm.stopBroadcast();
    }

    /**
     * @notice Pre-deposit tokens to collateral contracts for testing
     */
    function preDepositCollateral() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");

        address wada = vm.envAddress("FUJI_WADA");
        address tokenA = vm.envAddress("FUJI_TOKENA");
        address collateralWada = vm.envAddress("FUJI_COLLATERAL_WADA");
        address collateralTokenA = vm.envAddress("FUJI_COLLATERAL_TOKENA");

        vm.startBroadcast(deployerPrivateKey);

        uint256 depositAmount = 100000 * 1e18; // 100K tokens

        // Pre-deposit WADA for scenario 5 (native ADA -> collateral WADA)
        TestERC20(wada).approve(collateralWada, depositAmount);
        // Transfer directly to collateral contract
        TestERC20(wada).transfer(collateralWada, depositAmount);
        console.log("Pre-deposited 100K WADA to collateral contract");

        // Pre-deposit TokenA for scenario 7 (collateral-collateral)
        TestERC20(tokenA).approve(collateralTokenA, depositAmount);
        TestERC20(tokenA).transfer(collateralTokenA, depositAmount);
        console.log("Pre-deposited 100K TokenA to collateral contract");

        vm.stopBroadcast();
    }
}
