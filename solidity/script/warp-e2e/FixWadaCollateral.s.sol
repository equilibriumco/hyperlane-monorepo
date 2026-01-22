// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {HypERC20Collateral} from "contracts/token/HypERC20Collateral.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "./TestERC20.sol";

/**
 * @title FixWadaCollateral
 * @notice Redeploys Fuji WADA Collateral warp route with correct scale
 * @dev The original deployment used scale=1e12, but for bidirectional transfers
 *      between Fuji (18 decimals) and Cardano (6 decimals), the 18-decimal side
 *      should use scale=1 while the 6-decimal side uses scale=1e12.
 *
 *      With scale=1 on Fuji:
 *      - Fuji -> Cardano: wire = local * 1 = 10 * 1e18; Cardano divides by 1e12 = 10 * 1e6 lovelace = 10 ADA
 *      - Cardano -> Fuji: wire = local * 1e12 = 10 * 1e6 * 1e12 = 10 * 1e18; Fuji divides by 1 = 10 * 1e18 = 10 WADA
 */
contract FixWadaCollateral is Script {
    // Fuji Hyperlane infrastructure
    address constant FUJI_MAILBOX = 0x5b6CFf85442B851A8e6eaBd2A4E4507B5135B3B0;
    address constant FUJI_ISM = 0xD44036F1917bb13cB36a4ab1ad0F87324aacF1EB;

    // Cardano domain ID
    uint32 constant CARDANO_DOMAIN = 2003;

    // Cardano Native ADA #2 warp route (paired with WADA collateral)
    // Script hash: db711f4dc8a751e3615864e8a6d9b7aa1da24cb71ca0ba254cfdc4ba
    bytes32 constant CARDANO_NATIVE_ADA2 =
        0x02000000db711f4dc8a751e3615864e8a6d9b7aa1da24cb71ca0ba254cfdc4ba;

    // Correct scale: 1 (no scaling on the 18-decimal side)
    uint256 constant CORRECT_SCALE = 1;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);
        address wadaToken = vm.envAddress("FUJI_WADA");

        console.log("=== Fixing WADA Collateral with Correct Scale ===");
        console.log("Deployer:", deployer);
        console.log("WADA Token:", wadaToken);
        console.log("Mailbox:", FUJI_MAILBOX);
        console.log("ISM:", FUJI_ISM);
        console.log("Scale:", CORRECT_SCALE, "(was 1e12)");

        vm.startBroadcast(deployerPrivateKey);

        // 1. Deploy new collateral with correct scale
        HypERC20Collateral newCollateral = new HypERC20Collateral(
            wadaToken,
            CORRECT_SCALE,
            FUJI_MAILBOX
        );
        console.log("New WADA Collateral deployed:", address(newCollateral));

        // 2. Initialize the collateral
        newCollateral.initialize(
            address(0), // no hook
            FUJI_ISM, // ISM
            deployer
        );
        console.log("Collateral initialized");

        // 3. Enroll Cardano Native ADA #2 as remote router
        newCollateral.enrollRemoteRouter(CARDANO_DOMAIN, CARDANO_NATIVE_ADA2);
        console.log("Enrolled Cardano Native ADA #2 as router");
        console.log("  -> Cardano address:", vm.toString(CARDANO_NATIVE_ADA2));

        // 4. Pre-deposit WADA tokens (100K WADA)
        uint256 depositAmount = 100000 * 1e18;
        uint256 balance = IERC20(wadaToken).balanceOf(deployer);
        console.log("Deployer WADA balance:", balance);

        if (balance >= depositAmount) {
            IERC20(wadaToken).transfer(address(newCollateral), depositAmount);
            console.log("Pre-deposited 100K WADA to new collateral");
        } else if (balance > 0) {
            IERC20(wadaToken).transfer(address(newCollateral), balance);
            console.log("Pre-deposited all available WADA:", balance);
        } else {
            console.log(
                "WARNING: No WADA tokens to deposit. Use mintAndDeposit() to add liquidity."
            );
        }

        vm.stopBroadcast();

        console.log("\n=== Fix Complete ===");
        console.log("Update your .env with:");
        console.log(
            string.concat(
                "FUJI_COLLATERAL_WADA=",
                vm.toString(address(newCollateral))
            )
        );
        console.log("\nNew collateral scale:", newCollateral.scale());
    }

    /**
     * @notice Mint WADA tokens and deposit to collateral
     * @dev Use this if the deployer doesn't have WADA tokens
     */
    function mintAndDeposit() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);
        address wadaToken = vm.envAddress("FUJI_WADA");
        address collateral = vm.envAddress("FUJI_COLLATERAL_WADA");

        vm.startBroadcast(deployerPrivateKey);

        uint256 mintAmount = 100000 * 1e18; // 100K WADA
        TestERC20(wadaToken).mint(deployer, mintAmount);
        console.log("Minted 100K WADA to deployer");

        IERC20(wadaToken).transfer(collateral, mintAmount);
        console.log("Deposited 100K WADA to collateral:", collateral);

        vm.stopBroadcast();
    }
}
