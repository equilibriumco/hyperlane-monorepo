// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {HypERC20} from "contracts/token/HypERC20.sol";
import {MailboxClient} from "contracts/client/MailboxClient.sol";

/**
 * @title DeployWAdaRoute
 * @notice Deploys the Sepolia half of the demo warp route: ADA on Cardano,
 *         wADA on Sepolia.
 *
 * Run it after the Cardano side exists, since enrolling needs its address:
 *
 *     hyperlane-cardano warp deploy --token-type native --remote-decimals 6
 *
 * Reads everything from cardano/demo/docker/.env:
 *   SEPOLIA_SIGNER_KEY, SEPOLIA_MAILBOX, SEPOLIA_ISM, SEPOLIA_AGGREGATION_HOOK,
 *   CARDANO_NATIVE_WARP_ROUTE, CARDANO_DOMAIN (default 2003)
 *
 * The Cardano route is enrolled here, but the reverse enrollment is a separate
 * Cardano transaction:
 *
 *     hyperlane-cardano warp enroll-router --domain 11155111 --router <wADA>
 */
contract DeployWAdaRoute is Script {
    // ADA is 6 decimals and the Cardano route is deployed with
    // --remote-decimals 6, so the wire amount is already lovelace and no
    // rescaling is needed. Deploying wADA with 18 decimals here would require a
    // matching --remote-decimals 18 on the Cardano side; the two must agree or
    // every transfer silently arrives off by 10^12.
    uint8 internal constant DECIMALS = 6;
    uint256 internal constant SCALE_NUMERATOR = 1;
    uint256 internal constant SCALE_DENOMINATOR = 1;

    function run() external {
        uint256 deployerKey = vm.envUint("SEPOLIA_SIGNER_KEY");
        address mailbox = vm.envAddress("SEPOLIA_MAILBOX");
        address ism = vm.envAddress("SEPOLIA_ISM");
        address aggregationHook = vm.envAddress("SEPOLIA_AGGREGATION_HOOK");
        bytes32 cardanoRoute = vm.envBytes32("CARDANO_NATIVE_WARP_ROUTE");
        uint32 cardanoDomain = uint32(vm.envOr("CARDANO_DOMAIN", uint256(2003)));
        address owner = vm.addr(deployerKey);

        console.log("Deploying wADA synthetic on Sepolia");
        console.log("  Mailbox:  ", mailbox);
        console.log("  ISM:      ", ism);
        console.log("  Hook:     ", aggregationHook);
        console.log("  Domain:   ", cardanoDomain);
        console.log("  Cardano route:");
        console.logBytes32(cardanoRoute);

        vm.startBroadcast(deployerKey);

        HypERC20 wada = new HypERC20(
            DECIMALS,
            SCALE_NUMERATOR,
            SCALE_DENOMINATOR,
            mailbox
        );
        wada.initialize(
            0, // no initial supply; minted on receive
            "Wrapped ADA",
            "wADA",
            address(0), // hook set below
            ism,
            owner
        );

        // Without the aggregation hook the route posts to the merkle tree but
        // never pays the IGP, so the relayer sees an unpaid message and the
        // transfer stalls.
        MailboxClient(address(wada)).setHook(aggregationHook);
        wada.enrollRemoteRouter(cardanoDomain, cardanoRoute);

        vm.stopBroadcast();

        console.log("");
        console.log("SEPOLIA_SYNTHETIC_WADA=%s", address(wada));
        console.log("");
        console.log("Now enroll the reverse direction on Cardano:");
        console.log(
            "  hyperlane-cardano warp enroll-router --domain 11155111 --router %s",
            address(wada)
        );
    }
}
