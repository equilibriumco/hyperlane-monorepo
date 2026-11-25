#!/bin/sh
# Stand-in submitter for the unit tests, run through a link in a test's own
# folder: saves the request there and prints the reply kept there.
dir=$(dirname "$0")
cat > "$dir/request.json"
cat "$dir/reply"
