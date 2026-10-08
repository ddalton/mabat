#!/usr/bin/env bash
# Run a command against a throwaway PostgreSQL cluster, with MABAT_TEST_DATABASE_URL set.
#
#   scripts/with-postgres.sh                     # cargo test --workspace
#   scripts/with-postgres.sh cargo test -p mabat
#   scripts/with-postgres.sh cargo run --release -p mabat --example parity
#
# Needs the PostgreSQL server binaries (initdb, pg_ctl). Set PG_BIN to their directory if
# they are not on the PATH. The cluster lives in a temporary directory and is removed
# afterwards.
set -euo pipefail

if [[ -z "${PG_BIN:-}" ]]; then
  if command -v pg_ctl >/dev/null; then
    PG_BIN=$(dirname "$(command -v pg_ctl)")
  else
    PG_BIN=$(ls -d /opt/homebrew/opt/postgresql@*/bin /usr/lib/postgresql/*/bin 2>/dev/null | sort -V | tail -1 || true)
  fi
fi
if [[ ! -x "${PG_BIN:-}/initdb" ]]; then
  echo "PostgreSQL server binaries not found; set PG_BIN to the directory containing initdb and pg_ctl" >&2
  exit 1
fi

PORT=${MABAT_TEST_PORT:-55432}
DIR=$(mktemp -d "${TMPDIR:-/tmp}/mabat-pg.XXXXXX")
cleanup() {
  "$PG_BIN/pg_ctl" -D "$DIR/data" -m immediate stop >/dev/null 2>&1 || true
  rm -rf "$DIR"
}
trap cleanup EXIT

"$PG_BIN/initdb" -D "$DIR/data" -U mabat --auth=trust >/dev/null
"$PG_BIN/pg_ctl" -D "$DIR/data" -l "$DIR/server.log" -w \
  -o "-p $PORT -c listen_addresses=127.0.0.1 -k $DIR" start >/dev/null

export MABAT_TEST_DATABASE_URL="postgres://mabat@127.0.0.1:$PORT/postgres"
if [[ $# -eq 0 ]]; then
  set -- cargo test --workspace
fi
"$@"
