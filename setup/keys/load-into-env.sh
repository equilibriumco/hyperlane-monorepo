#!/usr/bin/env bash
# Copy the committed testing validator keys into ../.env, in place.
#
# These keys are throwaway and committed deliberately: they exist only to run
# this test bridge. Never point them at anything holding value.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
declare -A MAP=(
    [CARDANO_VALIDATOR_KEY_1]=validator-cardano-1
    [CARDANO_VALIDATOR_KEY_2]=validator-cardano-2
    [MIDNIGHT_VALIDATOR_KEY_1]=validator-midnight-1
    [MIDNIGHT_VALIDATOR_KEY_2]=validator-midnight-2
)
for var in "${!MAP[@]}"; do
    key=$(cat "${MAP[$var]}.key")
    sed -i "s|^${var}=.*|${var}=${key}|" ../.env
    echo "$var <- ${MAP[$var]}.key ($(cat "${MAP[$var]}.addr"))"
done
