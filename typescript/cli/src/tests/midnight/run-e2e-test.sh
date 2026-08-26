#!/usr/bin/env bash
set -e

# These tests need an externally managed devnet (node, indexer, proof server)
# and prove every state change for real, so they also need the full compiled
# contracts tree: prover keys are multi-GB and are not packaged with the SDK.

if [ -z "${HYPERLANE_MIDNIGHT_CONTRACTS}" ]; then
  echo "HYPERLANE_MIDNIGHT_CONTRACTS is not set."
  echo "Point it at a compiled Midnight contracts tree (the directory that"
  echo "contains night/, igp/, and validator-announce/, each with keys/ and"
  echo "zkir/) so deploys and circuit calls can prove."
  exit 1
fi

if [ ! -d "${HYPERLANE_MIDNIGHT_CONTRACTS}/night/keys" ]; then
  echo "HYPERLANE_MIDNIGHT_CONTRACTS (${HYPERLANE_MIDNIGHT_CONTRACTS}) has no night/keys directory."
  echo "The tests need a fully compiled contracts tree including prover keys."
  exit 1
fi

NODE_URL="${MIDNIGHT_E2E_NODE_URL:-http://127.0.0.1:9944}"
INDEXER_URL="${MIDNIGHT_E2E_INDEXER_URL:-http://127.0.0.1:8088/api/v3/graphql}"
PROOF_SERVER_URL="${MIDNIGHT_E2E_PROOF_SERVER_URL:-http://127.0.0.1:6300}"

function require_endpoint() {
  local name="$1"
  local url="$2"
  if ! curl -sf -o /dev/null --max-time 5 "$url"; then
    echo "Midnight devnet $name is not reachable at $url."
    echo "Start the local devnet (node + indexer + proof server, with raised"
    echo "contract limits) before running these tests."
    exit 1
  fi
}

require_endpoint "node" "$NODE_URL/health"
require_endpoint "proof server" "$PROOF_SERVER_URL/version"
# The indexer answers GraphQL POSTs only; any HTTP answer means it is up.
if ! curl -s -o /dev/null --max-time 5 "$INDEXER_URL"; then
  echo "Midnight devnet indexer is not reachable at $INDEXER_URL."
  echo "Start the local devnet before running these tests."
  exit 1
fi

echo "Running E2E tests"

if [ -n "${CLI_E2E_TEST}" ]; then
  echo "Running only ${CLI_E2E_TEST} test"
  pnpm mocha --config src/tests/midnight/.mocharc-e2e.json "src/tests/midnight/**/${CLI_E2E_TEST}.e2e-test.ts"
else
  pnpm mocha --config src/tests/midnight/.mocharc-e2e.json "src/tests/midnight/**/*.e2e-test.ts"
fi

echo "Completed E2E tests"
