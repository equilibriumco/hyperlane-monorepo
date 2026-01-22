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
 * @dev Each Cardano warp route now has a UNIQUE script hash, so different addresses per pair.
 *      Format: 0x02000000 prefix + 28-byte script hash
 */
contract EnrollCardanoRouters is Script {
    // Cardano domain ID
    uint32 constant CARDANO_DOMAIN = 2003;

    // Cardano warp route addresses - each route has a unique script hash now
    // Format: 0x02000000 prefix + 28-byte script hash

    // nativeAda script hash: 0ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb
    bytes32 constant CARDANO_NATIVE_ADA =
        0x020000000ea635a9db202792c36ceec3a6c9d4bea53a15eb481eb545b6976ddb;

    // collateralCtest script hash: b72f2aeeddc9d0203429ecdb0fb1d65129592a9da62757a6bee7e472
    bytes32 constant CARDANO_COLLATERAL_CTEST =
        0x02000000b72f2aeeddc9d0203429ecdb0fb1d65129592a9da62757a6bee7e472;

    // syntheticFtest script hash: 503a80b8f25f64f5375f7b1cac6e862dd333ec3dace7dc9544e9040c
    bytes32 constant CARDANO_SYNTHETIC_FTEST =
        0x02000000503a80b8f25f64f5375f7b1cac6e862dd333ec3dace7dc9544e9040c;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        console.log("Enrolling Cardano routers on Fuji warp routes");
        console.log("Deployer:", deployer);
        console.log("Cardano Domain:", CARDANO_DOMAIN);

        // Read deployed Fuji warp route addresses from environment
        address syntheticWCtest = vm.envAddress("FUJI_SYNTHETIC_WCTEST");
        address syntheticWAda = vm.envAddress("FUJI_SYNTHETIC_WADA");
        address collateralFtest = vm.envAddress("FUJI_COLLATERAL_FTEST");
        address collateralWada = vm.envAddress("FUJI_COLLATERAL_WADA");
        address collateralTokenA = vm.envAddress("FUJI_COLLATERAL_TOKENA");
        address nativeAvax = vm.envAddress("FUJI_NATIVE_AVAX");

        vm.startBroadcast(deployerPrivateKey);

        // Scenario 1: Fuji wCTEST synthetic <-> Cardano Collateral CTEST
        HypERC20(syntheticWCtest).enrollRemoteRouter(
            CARDANO_DOMAIN,
            CARDANO_COLLATERAL_CTEST
        );
        console.log(
            "Enrolled Cardano collateralCtest on wCTEST synthetic:",
            syntheticWCtest
        );
        console.log(
            "  -> Cardano address:",
            vm.toString(CARDANO_COLLATERAL_CTEST)
        );

        // Scenario 2: Fuji FTEST collateral <-> Cardano Synthetic wFTEST
        HypERC20Collateral(collateralFtest).enrollRemoteRouter(
            CARDANO_DOMAIN,
            CARDANO_SYNTHETIC_FTEST
        );
        console.log(
            "Enrolled Cardano syntheticFtest on FTEST collateral:",
            collateralFtest
        );
        console.log(
            "  -> Cardano address:",
            vm.toString(CARDANO_SYNTHETIC_FTEST)
        );

        // Scenario 3: Fuji wADA synthetic <-> Cardano Native ADA
        HypERC20(syntheticWAda).enrollRemoteRouter(
            CARDANO_DOMAIN,
            CARDANO_NATIVE_ADA
        );
        console.log(
            "Enrolled Cardano nativeAda on wADA synthetic:",
            syntheticWAda
        );
        console.log("  -> Cardano address:", vm.toString(CARDANO_NATIVE_ADA));

        // Scenario 4: Fuji AVAX native <-> Cardano Synthetic wAVAX (not deployed yet)
        // HypNative(payable(nativeAvax)).enrollRemoteRouter(CARDANO_DOMAIN, CARDANO_SYNTHETIC_AVAX);
        console.log(
            "Skipping AVAX native - Cardano synthetic wAVAX not deployed yet"
        );

        // Scenario 5: Fuji WADA collateral <-> Cardano Native ADA (alternative to scenario 3)
        HypERC20Collateral(collateralWada).enrollRemoteRouter(
            CARDANO_DOMAIN,
            CARDANO_NATIVE_ADA
        );
        console.log(
            "Enrolled Cardano nativeAda on WADA collateral:",
            collateralWada
        );
        console.log("  -> Cardano address:", vm.toString(CARDANO_NATIVE_ADA));

        // Scenario 7: TokenA collateral <- Cardano collateral TokenB (not deployed yet)
        // HypERC20Collateral(collateralTokenA).enrollRemoteRouter(CARDANO_DOMAIN, CARDANO_COLLATERAL_TOKENB);
        console.log(
            "Skipping TokenA collateral - Cardano collateral TokenB not deployed yet"
        );

        vm.stopBroadcast();

        console.log(
            "\n=== Fuji routers enrolled with unique Cardano addresses ==="
        );
    }
}
