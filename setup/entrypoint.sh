#!/usr/bin/env bash
# Renders the single agent-config template and execs one agent.
#
# Every agent reads the same rendered config; what differs per agent is passed
# as HYP_* env vars from docker-compose (HYP_ORIGINCHAINNAME, HYP_DB,
# HYP_METRICSPORT, ...). That is why there is one template here and not one per
# agent: the chain blocks are identical, only the agent's own knobs differ.
set -euo pipefail

# Values the template interpolates as raw JSON (not quoted strings) would
# produce invalid JSON if empty, so default them here rather than in the
# template.
: "${CARDANO_INDEX_FROM:?set CARDANO_INDEX_FROM to a block at or before the mailbox deployment}"
: "${MIDNIGHT_INDEX_FROM:=0}"
: "${LOG_LEVEL:=info}"
# Not `${VAR:=default}`: that form ends the expansion at the first unescaped
# `}`, which silently truncates a JSON default to `[{"type": ...` and drops the
# closing braces.
if [ -z "${GAS_ENFORCEMENT:-}" ]; then
    GAS_ENFORCEMENT='[{"type": "onChainFeeQuoting", "gasFraction": "1/1"}]'
fi
# The value has to be quoted in .env so `source .env` does not choke on the
# braces, but the loaders disagree about them: compose's `env_file` strips
# surrounding quotes, `docker run --env-file` passes them through. Strip one
# pair so the same .env works either way.
GAS_ENFORCEMENT="${GAS_ENFORCEMENT#[\"\']}"
GAS_ENFORCEMENT="${GAS_ENFORCEMENT%[\"\']}"
: "${MIDNIGHT_TOOLKIT_PATH:=/opt/midnight/relayer/bin/submit-handle}"
: "${CARDANO_BLOCKFROST_URL:=https://cardano-preview.blockfrost.io/api/v0}"
export CARDANO_INDEX_FROM MIDNIGHT_INDEX_FROM GAS_ENFORCEMENT LOG_LEVEL \
    MIDNIGHT_TOOLKIT_PATH CARDANO_BLOCKFROST_URL

AGENT_TYPE="${AGENT_TYPE:?set AGENT_TYPE to validator, relayer or scraper}"

# Cardano-origin validators announce from one shared wallet, so their announce
# transactions fight over the same collateral UTxOs. Staggering the second one
# is cheaper than teaching them to queue.
if [ "${START_DELAY_SECS:-0}" -gt 0 ]; then
    echo "waiting ${START_DELAY_SECS}s before starting $AGENT_TYPE"
    sleep "$START_DELAY_SECS"
fi

# The agent also opens a `config` directory relative to its working directory,
# which is /data so the Midnight toolkit has somewhere writable for its LevelDB.
mkdir -p "$PWD/config"

CONFIG=/tmp/agent-config.json
envsubst < /app/config/agent-config.json.tmpl > "$CONFIG"
node -e "JSON.parse(require('fs').readFileSync('$CONFIG','utf8'))" 2>/dev/null \
    || { echo "rendered config is not valid JSON — an env var is probably unset:"; cat "$CONFIG"; exit 1; }
export CONFIG_FILES="$CONFIG"
echo "rendered $CONFIG for $AGENT_TYPE"

case "$AGENT_TYPE" in
    validator)
        exec /app/validator "$@"
        ;;
    relayer)
        exec /app/relayer "$@"
        ;;
    scraper)
        # Idempotent: sea-orm skips migrations already recorded in the DB.
        echo "applying scraper migrations"
        /app/init-db
        exec /app/scraper "$@"
        ;;
    *)
        echo "unknown AGENT_TYPE: $AGENT_TYPE (want validator, relayer or scraper)"
        exit 1
        ;;
esac
