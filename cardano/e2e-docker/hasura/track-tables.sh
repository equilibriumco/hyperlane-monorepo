#!/usr/bin/env bash
# Expose the scraper database to hyperlane-explorer through Hasura.
#
# The explorer only ever queries two root fields (`message_view` and `domain`,
# see its features/messages/queries and features/chains/queries), so we track
# just those and grant the anonymous `public` role read access.
#
# Idempotent: re-tracking an already-tracked table is treated as success.
set -euo pipefail

HASURA_URL="${HASURA_URL:-http://localhost:8080}"
ADMIN_SECRET="${HASURA_ADMIN_SECRET:-hyperlane}"

metadata() {
    local payload="$1"
    local response
    response=$(curl -sS -X POST "$HASURA_URL/v1/metadata" \
        -H "X-Hasura-Admin-Secret: $ADMIN_SECRET" \
        -H 'Content-Type: application/json' \
        -d "$payload")

    # "already-tracked" / "already-exists" mean a previous run got there first.
    if grep -qE 'already-(tracked|exists)' <<<"$response"; then
        echo "  already applied"
        return 0
    fi
    if grep -q '"code"' <<<"$response"; then
        echo "  failed: $response" >&2
        return 1
    fi
    echo "  ok"
}

for table in message_view domain; do
    echo "Tracking $table"
    metadata "{
        \"type\": \"pg_track_table\",
        \"args\": {\"source\": \"default\", \"table\": {\"schema\": \"public\", \"name\": \"$table\"}}
    }"

    echo "Granting public select on $table"
    metadata "{
        \"type\": \"pg_create_select_permission\",
        \"args\": {
            \"source\": \"default\",
            \"table\": {\"schema\": \"public\", \"name\": \"$table\"},
            \"role\": \"public\",
            \"permission\": {\"columns\": \"*\", \"filter\": {}, \"allow_aggregations\": true}
        }
    }"
done

echo "Done. Point the explorer at $HASURA_URL/v1/graphql"
