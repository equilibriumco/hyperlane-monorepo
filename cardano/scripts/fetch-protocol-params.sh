#!/bin/bash
# Fetch and convert protocol parameters from Blockfrost to cardano-cli format

BLOCKFROST_API_KEY="${BLOCKFROST_API_KEY:-previewjtDS4mHuhJroFIX0BfOpVmMdnyTrWMfh}"
BLOCKFROST_URL="https://cardano-preview.blockfrost.io/api/v0"
OUTPUT_FILE="${1:-protocol.json}"

# Fetch from Blockfrost
PARAMS=$(curl -s -H "project_id: $BLOCKFROST_API_KEY" "$BLOCKFROST_URL/epochs/latest/parameters")

# Convert to cardano-cli format
jq '{
  txFeePerByte: .min_fee_a,
  txFeeFixed: .min_fee_b,
  maxBlockBodySize: .max_block_size,
  maxTxSize: .max_tx_size,
  maxBlockHeaderSize: .max_block_header_size,
  stakeAddressDeposit: (.key_deposit | tonumber),
  stakePoolDeposit: (.pool_deposit | tonumber),
  minPoolCost: (.min_pool_cost | tonumber),
  poolRetireMaxEpoch: .e_max,
  stakePoolTargetNum: .n_opt,
  poolPledgeInfluence: .a0,
  monetaryExpansion: .rho,
  treasuryCut: .tau,
  protocolVersion: {
    major: .protocol_major_ver,
    minor: .protocol_minor_ver
  },
  minUTxOValue: ((.min_utxo // "0") | tonumber),
  utxoCostPerByte: ((.coins_per_utxo_size // "4310") | tonumber),
  executionUnitPrices: {
    priceMemory: (.price_mem // 0.0577),
    priceSteps: (.price_step // 0.0000721)
  },
  maxTxExecutionUnits: {
    memory: ((.max_tx_ex_mem // "14000000") | tonumber),
    steps: ((.max_tx_ex_steps // "10000000000") | tonumber)
  },
  maxBlockExecutionUnits: {
    memory: ((.max_block_ex_mem // "62000000") | tonumber),
    steps: ((.max_block_ex_steps // "20000000000") | tonumber)
  },
  maxValueSize: ((.max_val_size // "5000") | tonumber),
  collateralPercentage: (.collateral_percent // 150),
  maxCollateralInputs: (.max_collateral_inputs // 3),
  poolVotingThresholds: {
    motionNoConfidence: 0.51,
    committeeNormal: 0.51,
    committeeNoConfidence: 0.51,
    hardForkInitiation: 0.51,
    ppSecurityGroup: 0.51
  },
  dRepVotingThresholds: {
    motionNoConfidence: 0.51,
    committeeNormal: 0.51,
    committeeNoConfidence: 0.51,
    updateToConstitution: 0.75,
    hardForkInitiation: 0.51,
    ppNetworkGroup: 0.51,
    ppEconomicGroup: 0.51,
    ppTechnicalGroup: 0.51,
    ppGovGroup: 0.51,
    treasuryWithdrawal: 0.51
  },
  committeeMinSize: 0,
  committeeMaxTermLength: 146,
  govActionLifetime: 6,
  govActionDeposit: 100000000000,
  dRepDeposit: 2000000,
  dRepActivity: 20,
  minFeeRefScriptCostPerByte: 44,
  costModels: (
    # Only include PlutusV3 and add missing parameters
    if .cost_models.PlutusV3 then
      {
        PlutusV3: (.cost_models.PlutusV3 + {
          "quotientInteger-memory-arguments-minimum": (.cost_models.PlutusV3["quotientInteger-memory-arguments-intercept"] // 0)
        })
      }
    else
      .cost_models
    end
  )
}' <<< "$PARAMS" > "$OUTPUT_FILE"

echo "Protocol parameters saved to $OUTPUT_FILE"
