#!/bin/bash
set -e

# This script processes a config template and substitutes environment variables
# before starting the agent

PROCESSED_CONFIG_DIR="/tmp/config"
mkdir -p "$PROCESSED_CONFIG_DIR"

# Determine which agent to run based on AGENT_TYPE env var
AGENT_TYPE="${AGENT_TYPE:-validator}"

# Determine the config template file based on agent type
case "$AGENT_TYPE" in
    validator)
        SOURCE_CONFIG="/app/config-templates/validator-cardano-preview.json"
        ;;
    relayer)
        SOURCE_CONFIG="/app/config-templates/relayer-cardano-fuji.json"
        ;;
    scraper)
        SOURCE_CONFIG="/app/config-templates/scraper.json"
        ;;
    *)
        echo "Unknown agent type: $AGENT_TYPE"
        echo "Valid types: validator, relayer, scraper"
        exit 1
        ;;
esac

# Process only the relevant config file
PROCESSED_CONFIG="$PROCESSED_CONFIG_DIR/$(basename "$SOURCE_CONFIG")"
echo "Processing config: $SOURCE_CONFIG"
envsubst < "$SOURCE_CONFIG" > "$PROCESSED_CONFIG"
echo "Processed config written to: $PROCESSED_CONFIG"

# Set CONFIG_FILES env var for the agent to load additional config
export CONFIG_FILES="$PROCESSED_CONFIG"

# Start the appropriate agent
case "$AGENT_TYPE" in
    validator)
        echo "Starting validator with CONFIG_FILES=$CONFIG_FILES"
        exec /app/validator "$@"
        ;;
    relayer)
        echo "Starting relayer with CONFIG_FILES=$CONFIG_FILES"
        exec /app/relayer "$@"
        ;;
    scraper)
        echo "Starting scraper with CONFIG_FILES=$CONFIG_FILES"
        exec /app/scraper "$@"
        ;;
esac
