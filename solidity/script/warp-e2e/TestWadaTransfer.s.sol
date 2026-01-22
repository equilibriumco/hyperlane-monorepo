// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {HypERC20Collateral} from "contracts/token/HypERC20Collateral.sol";
import {IERC20} from "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "./TestERC20.sol";

/**
 * @title TestWadaTransfer
 * @notice Sends a test WADA transfer from Fuji to Cardano using the fixed collateral (scale=1)
 */
contract TestWadaTransfer is Script {
    // Cardano domain ID
    uint32 constant CARDANO_DOMAIN = 2003;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);
        address wadaToken = vm.envAddress("FUJI_WADA");
        address collateral = vm.envAddress("FUJI_COLLATERAL_WADA");

        console.log("=== Testing WADA Transfer (Fixed Scale=1) ===");
        console.log("Deployer:", deployer);
        console.log("WADA Token:", wadaToken);
        console.log("Collateral (scale=1):", collateral);

        // Cardano recipient - payment credential in H256 format
        // Format: 0x01000000 (address type) + 28-byte payment key hash
        // Credential: 1212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89
        bytes32 cardanoRecipient = 0x010000001212a023380020f8c7b94b831e457b9ee65f009df9d1d588430dcc89;
        console.log("Cardano Recipient:", vm.toString(cardanoRecipient));

        // Send 5 WADA (5 * 1e18)
        uint256 amount = 5 * 1e18;
        console.log("Amount:", amount, "(5 WADA)");

        // Check scale
        uint256 scale = HypERC20Collateral(collateral).scale();
        console.log("Collateral Scale:", scale);
        require(scale == 1, "Scale must be 1 for correct decimal conversion");

        // Calculate expected wire amount
        uint256 wireAmount = amount * scale;
        console.log("Expected Wire Amount:", wireAmount);
        console.log("Expected Cardano Amount (lovelace):", wireAmount / 1e12);
        console.log("Expected Cardano Amount (ADA):", wireAmount / 1e18);

        vm.startBroadcast(deployerPrivateKey);

        // 1. Mint WADA tokens if needed
        uint256 balance = IERC20(wadaToken).balanceOf(deployer);
        console.log("Current WADA Balance:", balance);

        if (balance < amount) {
            uint256 mintAmount = amount - balance + 10 * 1e18; // Extra buffer
            TestERC20(wadaToken).mint(deployer, mintAmount);
            console.log("Minted WADA:", mintAmount);
        }

        // 2. Approve collateral to spend WADA
        IERC20(wadaToken).approve(collateral, amount);
        console.log("Approved collateral to spend WADA");

        // 3. Send transfer
        bytes32 messageId = HypERC20Collateral(collateral).transferRemote{
            value: 1
        }(CARDANO_DOMAIN, cardanoRecipient, amount);
        console.log("Transfer sent! Message ID:", vm.toString(messageId));

        vm.stopBroadcast();

        console.log("\n=== Transfer Initiated ===");
        console.log("Message ID:", vm.toString(messageId));
        console.log("Amount: 5 WADA -> 5 ADA on Cardano");
        console.log("\nMonitor the relayer to see the transfer complete.");
    }

    /**
     * @notice Alternative: send to a specific Cardano address
     */
    function sendToAddress(bytes32 recipient, uint256 amount) external {
        uint256 deployerPrivateKey = vm.envUint("FUJI_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);
        address wadaToken = vm.envAddress("FUJI_WADA");
        address collateral = vm.envAddress("FUJI_COLLATERAL_WADA");

        vm.startBroadcast(deployerPrivateKey);

        // Approve and transfer
        IERC20(wadaToken).approve(collateral, amount);
        bytes32 messageId = HypERC20Collateral(collateral).transferRemote{
            value: 0
        }(CARDANO_DOMAIN, recipient, amount);

        console.log("Transfer sent! Message ID:", vm.toString(messageId));

        vm.stopBroadcast();
    }
}
