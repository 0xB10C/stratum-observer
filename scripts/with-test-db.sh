#!/usr/bin/env bash
#
# Runs a command against a temporary PostgreSQL database.
#
# The script starts a throw-away PostgreSQL server, creates a database,
# exports TEST_DATABASE_URL and runs the given command (default: cargo test).
# The server and all its data are removed again when the command finishes.
#
# Usage (needs initdb, pg_ctl and createdb in PATH, e.g. via nix-shell):
#
#   scripts/with-test-db.sh                   # runs 'cargo test'
#   scripts/with-test-db.sh cargo test --all  # runs the given command
#   TEST_DB_PORT=5555 scripts/with-test-db.sh # uses another port

set -euo pipefail

port="${TEST_DB_PORT:-54329}"
dbname="stratum_observer_test"

# PostgreSQL only allows unix socket paths of up to 107 bytes, so keep the
# directory name short.
dir="$(mktemp -d /tmp/so-pg-XXXXXX)"

cleanup() {
    if [ -d "$dir/data" ]; then
        pg_ctl -D "$dir/data" -m immediate -w stop > /dev/null 2>&1 || true
    fi
    rm -rf "$dir"
}
trap cleanup EXIT

echo "Setting up a temporary PostgreSQL database in $dir"
initdb -D "$dir/data" -U postgres --auth=trust > /dev/null
pg_ctl -D "$dir/data" -o "-k $dir -p $port -h 127.0.0.1" -l "$dir/log.txt" -w start > /dev/null
createdb -h 127.0.0.1 -p "$port" -U postgres "$dbname"

export TEST_DATABASE_URL="postgres://postgres@127.0.0.1:$port/$dbname"
echo "TEST_DATABASE_URL=$TEST_DATABASE_URL"

if [ "$#" -eq 0 ]; then
    set -- cargo test
fi

status=0
"$@" || status=$?
exit $status
