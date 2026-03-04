// SPDX-License-Identifier: MIT
pragma solidity ^0.8.22;

import "forge-std/Script.sol";
import "forge-std/console.sol";

import {StorageGasOracle} from "contracts/hooks/igp/StorageGasOracle.sol";
import {InterchainGasPaymaster} from "contracts/hooks/igp/InterchainGasPaymaster.sol";
import {IGasOracle} from "contracts/interfaces/IGasOracle.sol";

/**
 * @title DeploySepoliaIGP
 * @notice Deploys StorageGasOracle + InterchainGasPaymaster on an EVM chain
 *         for E2E testing with Cardano. We need our own IGP because the
 *         official Hyperlane IGP is owned by the team and we can't add
 *         Cardano (domain 2003) oracle data.
 *
 * Required environment variables:
 *   - EVM_SIGNER_KEY: Private key for transactions
 *
 * Optional environment variables:
 *   - CARDANO_DOMAIN: Cardano domain ID (default: 2003)
 *   - CARDANO_GAS_PRICE: Cardano gas price in lovelace per byte (default: 44)
 *   - CARDANO_TOKEN_EXCHANGE_RATE: tokenExchangeRate for IGP formula (default: 1.395e18)
 *   - CARDANO_GAS_OVERHEAD: gasOverhead for destination (default: 86000)
 */
contract DeploySepoliaIGP is Script {
    uint32 constant DEFAULT_CARDANO_DOMAIN = 2003;

    // Cardano min_fee_a (lovelace per byte)
    uint128 constant DEFAULT_GAS_PRICE = 44;

    // Derived from: 1 ETH = 7170.79 ADA
    // lovelace_cost = gasOverhead * gasPrice = 86000 * 44 = 3,784,000 lovelace = 3.784 ADA
    // ETH equiv = 3.784 / 7170.79 = 5.278e14 wei
    // tokenExchangeRate = 5.278e14 * 1e10 / (86000 * 44) = 1.395e18
    uint128 constant DEFAULT_TOKEN_EXCHANGE_RATE = 1_395_000_000_000_000_000;

    // Base cost in Cardano gas units (fee + verified_msg overhead)
    uint96 constant DEFAULT_GAS_OVERHEAD = 86_000;

    function run() external {
        uint256 deployerPrivateKey = vm.envUint("EVM_SIGNER_KEY");
        address deployer = vm.addr(deployerPrivateKey);

        uint32 cardanoDomain = uint32(
            vm.envOr("CARDANO_DOMAIN", uint256(DEFAULT_CARDANO_DOMAIN))
        );
        uint128 gasPrice = uint128(
            vm.envOr("CARDANO_GAS_PRICE", uint256(DEFAULT_GAS_PRICE))
        );
        uint128 tokenExchangeRate = uint128(
            vm.envOr(
                "CARDANO_TOKEN_EXCHANGE_RATE",
                uint256(DEFAULT_TOKEN_EXCHANGE_RATE)
            )
        );
        uint96 gasOverhead = uint96(
            vm.envOr("CARDANO_GAS_OVERHEAD", uint256(DEFAULT_GAS_OVERHEAD))
        );

        console.log("Deploying IGP + StorageGasOracle");
        console.log("Deployer:", deployer);
        console.log("Cardano Domain:", cardanoDomain);
        console.log("Gas Price:", gasPrice);
        console.log("Token Exchange Rate:", tokenExchangeRate);
        console.log("Gas Overhead:", gasOverhead);

        vm.startBroadcast(deployerPrivateKey);

        StorageGasOracle oracle = new StorageGasOracle();
        console.log("StorageGasOracle deployed:", address(oracle));

        InterchainGasPaymaster igp = new InterchainGasPaymaster();
        igp.initialize(deployer, deployer);
        console.log("InterchainGasPaymaster deployed:", address(igp));

        // Configure oracle with Cardano gas data
        StorageGasOracle.RemoteGasDataConfig[]
            memory configs = new StorageGasOracle.RemoteGasDataConfig[](1);
        configs[0] = StorageGasOracle.RemoteGasDataConfig({
            remoteDomain: cardanoDomain,
            tokenExchangeRate: tokenExchangeRate,
            gasPrice: gasPrice
        });
        oracle.setRemoteGasDataConfigs(configs);
        console.log("Oracle configured for Cardano domain");

        // Configure IGP with oracle + overhead
        InterchainGasPaymaster.GasParam[]
            memory gasParams = new InterchainGasPaymaster.GasParam[](1);
        gasParams[0] = InterchainGasPaymaster.GasParam({
            remoteDomain: cardanoDomain,
            config: InterchainGasPaymaster.DomainGasConfig({
                gasOracle: IGasOracle(address(oracle)),
                gasOverhead: gasOverhead
            })
        });
        igp.setDestinationGasConfigs(gasParams);
        console.log("IGP destination gas config set");

        vm.stopBroadcast();

        console.log("\n=== Environment Variables ===");
        console.log(
            string.concat(
                "EVM_STORAGE_GAS_ORACLE=",
                vm.toString(address(oracle))
            )
        );
        console.log(string.concat("EVM_IGP=", vm.toString(address(igp))));
    }

    /**
     * @notice Update oracle gas data (e.g. after exchange rate change)
     */
    function updateOracleGasData() external {
        uint256 deployerPrivateKey = vm.envUint("EVM_SIGNER_KEY");
        address oracleAddr = vm.envAddress("EVM_STORAGE_GAS_ORACLE");
        uint32 cardanoDomain = uint32(
            vm.envOr("CARDANO_DOMAIN", uint256(DEFAULT_CARDANO_DOMAIN))
        );
        uint128 gasPrice = uint128(vm.envUint("CARDANO_GAS_PRICE"));
        uint128 tokenExchangeRate = uint128(
            vm.envUint("CARDANO_TOKEN_EXCHANGE_RATE")
        );

        console.log("Updating oracle gas data");
        console.log("Oracle:", oracleAddr);
        console.log("Gas Price:", gasPrice);
        console.log("Token Exchange Rate:", tokenExchangeRate);

        vm.startBroadcast(deployerPrivateKey);

        StorageGasOracle.RemoteGasDataConfig[]
            memory configs = new StorageGasOracle.RemoteGasDataConfig[](1);
        configs[0] = StorageGasOracle.RemoteGasDataConfig({
            remoteDomain: cardanoDomain,
            tokenExchangeRate: tokenExchangeRate,
            gasPrice: gasPrice
        });
        StorageGasOracle(oracleAddr).setRemoteGasDataConfigs(configs);

        vm.stopBroadcast();

        console.log("Oracle gas data updated");
    }
}
