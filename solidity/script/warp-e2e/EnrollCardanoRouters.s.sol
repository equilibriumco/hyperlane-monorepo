// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {HypERC20} from "contracts/token/HypERC20.sol";
import {HypERC20Collateral} from "contracts/token/HypERC20Collateral.sol";
import {HypNative} from "contracts/token/HypNative.sol";

/**
 * @title EnrollCardanoRouters
 * @notice Enrolls the Cardano warp route addresses as remote routers on Fuji warp routes
 * @dev Each Cardano warp route has a UNIQUE script hash, so different addresses per pair.
 *      Format: 0x02000000 prefix + 28-byte script hash
 *
 * Required environment variables:
 *   - FUJI_SIGNER_KEY: Private key for Fuji transactions
 *   - FUJI_SYNTHETIC_WCTEST: Fuji wCTEST synthetic warp route address
 *   - FUJI_SYNTHETIC_WADA: Fuji wADA synthetic warp route address
 *   - FUJI_COLLATERAL_FTEST: Fuji FTEST collateral warp route address
 *   - FUJI_COLLATERAL_WADA: Fuji WADA collateral warp route address
 *   - CARDANO_NATIVE_ADA: Cardano Native ADA warp route (H256 format)
 *   - CARDANO_COLLATERAL_CTEST: Cardano Collateral CTEST warp route (H256 format)
 *   - CARDANO_SYNTHETIC_FTEST: Cardano Synthetic FTEST warp route (H256 format)
 *
 * Optional environment variables:
 *   - CARDANO_DOMAIN: Cardano domain ID (default: 2003 for Preview)
 *
 * Cardano H256 address format: 0x02000000 + 28-byte script hash
 * Example: 0x020000000ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb
 */
contract EnrollCardanoRouters is Script {
    // Default Cardano domain ID (Preview testnet)
    uint32 constant DEFAULT_CARDANO_DOMAIN = 2003;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        // Read Cardano domain (optional, defaults to Preview)
        uint32 cardanoDomain = uint32(
            vm.envOr("CARDANO_DOMAIN", uint256(DEFAULT_CARDANO_DOMAIN))
        );

        // Read Cardano warp route addresses from environment (required)
        bytes32 cardanoNativeAda = vm.envBytes32("CARDANO_NATIVE_ADA");
        bytes32 cardanoCollateralCtest = vm.envBytes32(
            "CARDANO_COLLATERAL_CTEST"
        );
        bytes32 cardanoSyntheticFtest = vm.envBytes32(
            "CARDANO_SYNTHETIC_FTEST"
        );

        console.log("Enrolling Cardano routers on Fuji warp routes");
        console.log("Deployer:", deployer);
        console.log("Cardano Domain:", cardanoDomain);
        console.log("Cardano Native ADA:", vm.toString(cardanoNativeAda));
        console.log(
            "Cardano Collateral CTEST:",
            vm.toString(cardanoCollateralCtest)
        );
        console.log(
            "Cardano Synthetic FTEST:",
            vm.toString(cardanoSyntheticFtest)
        );

        // Read deployed Fuji warp route addresses from environment
        address syntheticWCtest = vm.envAddress("FUJI_SYNTHETIC_WCTEST");
        address syntheticWAda = vm.envAddress("FUJI_SYNTHETIC_WADA");
        address collateralFtest = vm.envAddress("FUJI_COLLATERAL_FTEST");
        address collateralWada = vm.envAddress("FUJI_COLLATERAL_WADA");

        vm.startBroadcast(deployerPrivateKey);

        // Scenario 1: Fuji wCTEST synthetic <-> Cardano Collateral CTEST
        HypERC20(syntheticWCtest).enrollRemoteRouter(
            cardanoDomain,
            cardanoCollateralCtest
        );
        console.log(
            "Enrolled Cardano collateralCtest on wCTEST synthetic:",
            syntheticWCtest
        );
        console.log(
            "  -> Cardano address:",
            vm.toString(cardanoCollateralCtest)
        );

        // Scenario 2: Fuji FTEST collateral <-> Cardano Synthetic wFTEST
        HypERC20Collateral(collateralFtest).enrollRemoteRouter(
            cardanoDomain,
            cardanoSyntheticFtest
        );
        console.log(
            "Enrolled Cardano syntheticFtest on FTEST collateral:",
            collateralFtest
        );
        console.log(
            "  -> Cardano address:",
            vm.toString(cardanoSyntheticFtest)
        );

        // Scenario 3: Fuji wADA synthetic <-> Cardano Native ADA
        HypERC20(syntheticWAda).enrollRemoteRouter(
            cardanoDomain,
            cardanoNativeAda
        );
        console.log(
            "Enrolled Cardano nativeAda on wADA synthetic:",
            syntheticWAda
        );
        console.log("  -> Cardano address:", vm.toString(cardanoNativeAda));

        // Scenario 5: Fuji WADA collateral <-> Cardano Native ADA (alternative to scenario 3)
        HypERC20Collateral(collateralWada).enrollRemoteRouter(
            cardanoDomain,
            cardanoNativeAda
        );
        console.log(
            "Enrolled Cardano nativeAda on WADA collateral:",
            collateralWada
        );
        console.log("  -> Cardano address:", vm.toString(cardanoNativeAda));

        vm.stopBroadcast();

        console.log("\n=== Fuji routers enrolled with Cardano addresses ===");
    }

    /**
     * @notice Enroll a single Cardano router on a specific Fuji warp route
     * @dev Useful for enrolling routers one at a time or for routes not covered by run()
     *
     * Required environment variables:
     *   - FUJI_SIGNER_KEY: Private key for Fuji transactions
     *   - FUJI_WARP_ROUTE: Fuji warp route address to enroll on
     *   - CARDANO_ROUTER: Cardano warp route address (H256 format)
     *   - CARDANO_DOMAIN: Cardano domain ID (optional, default: 2003)
     */
    function enrollSingle() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");

        uint32 cardanoDomain = uint32(
            vm.envOr("CARDANO_DOMAIN", uint256(DEFAULT_CARDANO_DOMAIN))
        );
        address fujiWarpRoute = vm.envAddress("FUJI_WARP_ROUTE");
        bytes32 cardanoRouter = vm.envBytes32("CARDANO_ROUTER");

        console.log("Enrolling single Cardano router");
        console.log("Fuji Warp Route:", fujiWarpRoute);
        console.log("Cardano Domain:", cardanoDomain);
        console.log("Cardano Router:", vm.toString(cardanoRouter));

        vm.startBroadcast(deployerPrivateKey);

        // Use HypERC20 interface (works for all TokenRouter types)
        HypERC20(fujiWarpRoute).enrollRemoteRouter(
            cardanoDomain,
            cardanoRouter
        );

        vm.stopBroadcast();

        console.log("\n=== Router enrolled successfully ===");
    }
}
